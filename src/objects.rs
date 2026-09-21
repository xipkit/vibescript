use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge, MAX_VALUE_DEPTH, Memory},
    hash::Hash,
    namespace::Namespace,
    value::Kind,
};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Heap {
    owner: Arc<Memory>,
    data: Mutex<Data>,
    _header: Option<Charge>,
}

struct Data {
    entries: Buffer<Entry>,
    classes: Buffer<Arc<Namespace>>,
    imports: Buffer<(u64, Arc<Identity>)>,
    allocations: usize,
    pending: Buffer<usize>,
    /// Container frames for garbage-collection marking. Capacity is reserved
    /// when fields are written so marking never allocates, including during
    /// cleanup after the budget is exhausted.
    scratch: Buffer<(Value, usize)>,
}

/// Lazily copied elements of one container being mapped between handle kinds.
enum Mapped {
    Array(Buffer<Value>),
    Hash(Buffer<(Value, Value)>),
}

struct MapFrame<'a> {
    source: &'a Value,
    position: usize,
    mapped: Option<Mapped>,
}

impl<'a> MapFrame<'a> {
    fn new(source: &'a Value) -> Self {
        Self {
            source,
            position: 0,
            mapped: None,
        }
    }

    fn child(&self) -> Option<&'a Value> {
        let source: &'a Value = self.source;
        match &source.0 {
            Kind::Array(array) => array.buffer.data.get(self.position),
            Kind::Hash(hash) => hash.buffer.data.get(self.position).map(|(_, value)| value),
            _ => unreachable!(),
        }
    }

    /// Replaces one element, copying the source container on first use.
    fn assign(&mut self, ctx: &mut CallContext, index: usize, value: Value) -> Result<()> {
        match &self.source.0 {
            Kind::Array(array) => {
                if self.mapped.is_none() {
                    let mut buffer = Buffer::with_capacity(ctx, array.buffer.data.len())?;
                    buffer.extend(ctx, &array.buffer.data)?;
                    self.mapped = Some(Mapped::Array(buffer));
                }
                let Some(Mapped::Array(buffer)) = &mut self.mapped else {
                    unreachable!()
                };
                buffer.data[index] = value;
            }
            Kind::Hash(hash) => {
                if self.mapped.is_none() {
                    let mut buffer = Buffer::with_capacity(ctx, hash.buffer.data.len())?;
                    buffer.extend(ctx, &hash.buffer.data)?;
                    self.mapped = Some(Mapped::Hash(buffer));
                }
                let Some(Mapped::Hash(buffer)) = &mut self.mapped else {
                    unreachable!()
                };
                buffer.data[index].1 = value;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn finish(self, ctx: &mut CallContext) -> Result<Option<Value>> {
        match (&self.source.0, self.mapped) {
            (_, None) => Ok(None),
            (Kind::Array(_), Some(Mapped::Array(buffer))) => {
                Value::from_array(ctx, buffer).map(Some)
            }
            (Kind::Hash(hash), Some(Mapped::Hash(buffer))) => {
                let mut mapped = Hash::from_entries(ctx, buffer)?;
                mapped.object = hash.object;
                mapped.tag = hash.tag;
                Value::from_hash(ctx, mapped).map(Some)
            }
            _ => unreachable!(),
        }
    }
}

struct Entry {
    internal: Arc<Instance>,
    fields: Hash,
}

struct Identity {
    id: u64,
    nominal_scope: u64,
    roots: AtomicUsize,
    marked: AtomicBool,
    slot: AtomicUsize,
    class: Arc<Namespace>,
    _header: Option<Charge>,
}

enum Owner {
    External(Arc<Heap>),
    Internal(Weak<Heap>),
}

pub(crate) struct Instance {
    identity: Arc<Identity>,
    owner: Owner,
    _header: Option<Charge>,
}

impl Drop for Instance {
    fn drop(&mut self) {
        if matches!(self.owner, Owner::External(_)) {
            self.identity.roots.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Instance")
            .field(&self.identity.class.definition.name)
            .finish()
    }
}

impl Instance {
    /// Supplies stable scope identity without retaining the mutable instance heap.
    pub(crate) fn checking_id(&self) -> u64 {
        self.identity.id
    }

    /// Identifies a type scope independently of copies of its mutable state.
    pub(crate) fn nominal_scope(&self) -> u64 {
        self.identity.nominal_scope
    }

    pub fn class(&self) -> &Arc<Namespace> {
        &self.identity.class
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }

    pub(crate) fn alive(&self) -> bool {
        self.identity.slot.load(Ordering::Relaxed) != usize::MAX
    }

    pub(crate) fn rooted(&self) -> bool {
        matches!(self.owner, Owner::External(_))
    }

    fn heap(&self) -> Result<Arc<Heap>> {
        match &self.owner {
            Owner::External(heap) => Ok(heap.clone()),
            Owner::Internal(heap) => heap
                .upgrade()
                .ok_or_else(|| Error::new(ErrorKind::Type, "expired instance reference")),
        }
    }

    fn writable_heap(&self, ctx: &CallContext) -> Result<Arc<Heap>> {
        let heap = self.heap()?;
        if !Arc::ptr_eq(&heap.owner, &ctx.identity()) {
            return Err(Error::new(
                ErrorKind::Type,
                "instance belongs to a different invocation",
            ));
        }
        Ok(heap)
    }
}

pub(crate) fn unroot(instance: &Arc<Instance>) -> Result<Arc<Instance>> {
    let heap = instance.heap()?;
    let data = heap.data.lock().unwrap();
    data.entries
        .data
        .get(instance.identity.slot.load(Ordering::Relaxed))
        .filter(|entry| entry.internal.same(instance))
        .map(|entry| entry.internal.clone())
        .ok_or_else(|| Error::new(ErrorKind::Type, "expired instance reference"))
}

impl PartialEq for Instance {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}

impl Eq for Instance {}

fn local(ctx: &mut CallContext) -> Result<Arc<Heap>> {
    if let Some(heap) = &ctx.objects {
        return Ok(heap.clone());
    }
    let header = ctx.reserve(size_of::<Heap>() + 2 * size_of::<usize>())?;
    let heap = Arc::new(Heap {
        owner: ctx.identity(),
        data: Mutex::new(Data {
            entries: Buffer::empty(),
            classes: Buffer::empty(),
            imports: Buffer::empty(),
            allocations: 0,
            pending: Buffer::empty(),
            scratch: Buffer::empty(),
        }),
        _header: header,
    });
    ctx.objects = Some(heap.clone());
    Ok(heap)
}

pub(crate) fn environment(ctx: &mut CallContext) -> Result<Arc<Instance>> {
    new(ctx, environment_class())
}

/// Captures an unbound program's state without changing its declaration types.
pub(crate) fn snapshot_environment(ctx: &mut CallContext) -> Result<Arc<Instance>> {
    new_scoped(ctx, environment_class(), Some(0))
}

fn environment_class() -> &'static Arc<Namespace> {
    static TEMPLATE: std::sync::OnceLock<Arc<Namespace>> = std::sync::OnceLock::new();
    TEMPLATE.get_or_init(|| {
        Namespace::untracked(crate::namespace::Definition::new(
            usize::MAX,
            "<environment>".into(),
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
            None,
        ))
    })
}

pub(crate) fn new(ctx: &mut CallContext, class: &Arc<Namespace>) -> Result<Arc<Instance>> {
    new_scoped(ctx, class, None)
}

fn new_scoped(
    ctx: &mut CallContext,
    class: &Arc<Namespace>,
    nominal_scope: Option<u64>,
) -> Result<Arc<Instance>> {
    let heap = local(ctx)?;
    if heap.data.lock().unwrap().allocations >= 32 {
        collect(ctx, &heap, false)?;
    }
    let imported = (class.environment.is_some() || ctx.snapshot_namespaces.is_some())
        .then(|| Namespace::import(ctx, class))
        .transpose()?;
    let class = imported.as_ref().unwrap_or(class);
    let known = {
        let data = heap.data.lock().unwrap();
        let mut known = None;
        for candidate in &data.classes.data {
            ctx.charge(1)?;
            if candidate.same_binding(class) {
                known = Some(candidate.clone());
                break;
            }
        }
        known
    };
    let class = if let Some(known) = known {
        known
    } else {
        // Importing a captured environment can allocate objects in this heap.
        let class = Namespace::import(ctx, class)?;
        let mut data = heap.data.lock().unwrap();
        let class = data.namespace(ctx, &heap, &class, true)?.unwrap_or(class);
        data.classes.push(ctx, class.clone())?;
        class
    };
    let identity_header = ctx.reserve(size_of::<Identity>() + 2 * size_of::<usize>())?;
    let internal_header = ctx.reserve(size_of::<Instance>() + 2 * size_of::<usize>())?;
    let external_header = ctx.reserve(size_of::<Instance>() + 2 * size_of::<usize>())?;
    let mut data = heap.data.lock().unwrap();
    let Ok(id) = NEXT_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        return ctx.fail(ErrorKind::Memory, "instance identity space exhausted");
    };
    let needed = data.entries.data.len() + 1;
    if needed > data.pending.data.capacity() {
        data.pending
            .ensure(ctx, needed.max(8).next_power_of_two())?;
    }
    let identity = Arc::new(Identity {
        id,
        nominal_scope: nominal_scope.unwrap_or(id),
        roots: AtomicUsize::new(1),
        marked: AtomicBool::new(false),
        slot: AtomicUsize::new(data.entries.data.len()),
        class,
        _header: identity_header,
    });
    let internal = Arc::new(Instance {
        identity: identity.clone(),
        owner: Owner::Internal(Arc::downgrade(&heap)),
        _header: internal_header,
    });
    let external = Arc::new(Instance {
        identity,
        owner: Owner::External(heap.clone()),
        _header: external_header,
    });
    data.entries.push(
        ctx,
        Entry {
            internal,
            fields: Hash::empty(),
        },
    )?;
    data.allocations += 1;
    Ok(external)
}

impl Data {
    fn root(
        &self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        identity: &Arc<Identity>,
    ) -> Result<Arc<Instance>> {
        self.entries
            .data
            .get(identity.slot.load(Ordering::Relaxed))
            .filter(|entry| Arc::ptr_eq(identity, &entry.internal.identity))
            .ok_or_else(|| Error::new(ErrorKind::Type, "expired instance reference"))?;
        let header = ctx.reserve(size_of::<Instance>() + 2 * size_of::<usize>())?;
        identity.roots.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Instance {
            identity: identity.clone(),
            owner: Owner::External(heap.clone()),
            _header: header,
        }))
    }

    fn instance(
        &self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        instance: &Arc<Instance>,
        internal: bool,
    ) -> Result<Option<Arc<Instance>>> {
        if !Arc::ptr_eq(heap, &instance.heap()?) {
            return Err(Error::new(
                ErrorKind::Type,
                "instance field belongs to a different invocation",
            ));
        }
        match (&instance.owner, internal) {
            (Owner::Internal(_), true) | (Owner::External(_), false) => Ok(None),
            (_, true) => {
                let slot = instance.identity.slot.load(Ordering::Relaxed);
                Ok(Some(self.entries.data[slot].internal.clone()))
            }
            (_, false) => self.root(ctx, heap, &instance.identity).map(Some),
        }
    }

    fn namespace(
        &self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        namespace: &Arc<Namespace>,
        internal: bool,
    ) -> Result<Option<Arc<Namespace>>> {
        let Some(environment) = &namespace.environment else {
            return Ok(None);
        };
        ctx.charge(1)?;
        self.instance(ctx, heap, environment, internal)?
            .map(|environment| Namespace::with_environment(ctx, namespace, environment))
            .transpose()
    }

    /// Converts instance references inside a field value between internal and
    /// external handles. Containers are copied only along paths that change;
    /// the walk keeps its frames in a metered buffer sized from the cached
    /// height and never recurses natively. Only value containers count toward depth.
    fn map(
        &mut self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        value: &Value,
        internal: bool,
    ) -> Result<Option<Value>> {
        ctx.charge(1)?;
        nesting(ctx, value.depth())?;
        if !matches!(value.0, Kind::Array(_) | Kind::Hash(_)) {
            return self.leaf(ctx, heap, value, internal);
        }
        let mut frames = Buffer::with_capacity(ctx, value.depth().min(MAX_VALUE_DEPTH))?;
        frames.push(ctx, MapFrame::new(value))?;
        loop {
            let frame = frames.data.last_mut().unwrap();
            let Some(child) = frame.child() else {
                let finished = frames.data.pop().unwrap();
                let mapped = finished.finish(ctx)?;
                let Some(parent) = frames.data.last_mut() else {
                    return Ok(mapped);
                };
                if let Some(mapped) = mapped {
                    let index = parent.position - 1;
                    parent.assign(ctx, index, mapped)?;
                }
                continue;
            };
            let index = frame.position;
            frame.position += 1;
            ctx.charge(1)?;
            if matches!(child.0, Kind::Array(_) | Kind::Hash(_)) {
                frames.push(ctx, MapFrame::new(child))?;
            } else if let Some(mapped) = self.leaf(ctx, heap, child, internal)? {
                frames.data.last_mut().unwrap().assign(ctx, index, mapped)?;
            }
        }
    }

    fn leaf(
        &self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        value: &Value,
        internal: bool,
    ) -> Result<Option<Value>> {
        match &value.0 {
            Kind::Function(function) => self
                .instance(ctx, heap, &function.environment, internal)?
                .map(|environment| {
                    crate::exports::Function::with_environment(ctx, function, environment)
                        .map(|value| Value(Kind::Function(value)))
                })
                .transpose(),
            Kind::Instance(instance) => self
                .instance(ctx, heap, instance, internal)
                .map(|value| value.map(|value| Value(Kind::Instance(value)))),
            Kind::Namespace(namespace) => self
                .namespace(ctx, heap, namespace, internal)
                .map(|value| value.map(|value| Value(Kind::Namespace(value)))),
            _ => Ok(None),
        }
    }

    /// Reserves marking frames for a field value before it is stored.
    fn reserve_scratch(&mut self, ctx: &mut CallContext, depth: usize) -> Result<()> {
        if depth > self.scratch.data.capacity() {
            self.scratch.ensure(ctx, depth.max(8).next_power_of_two())?;
        }
        Ok(())
    }
}

fn nesting(ctx: &mut CallContext, depth: usize) -> Result<()> {
    if depth > MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "instance field nesting too deep");
    }
    Ok(())
}

pub(crate) fn field(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    name: &str,
) -> Result<Option<Value>> {
    let heap = instance.heap()?;
    let mut data = heap.data.lock().unwrap();
    let fields = &data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
    let Some(index) = fields.find(ctx, name.as_bytes())? else {
        return Ok(None);
    };
    let value = fields.buffer.data[index].1.clone();
    Ok(Some(data.map(ctx, &heap, &value, false)?.unwrap_or(value)))
}

pub(crate) fn children(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    values: &mut Buffer<Value>,
) -> Result<()> {
    let heap = instance.heap()?;
    let data = heap.data.lock().unwrap();
    if let Some(environment) = &instance.class().environment {
        ctx.charge(1)?;
        values.push(ctx, Value(Kind::Instance(environment.clone())))?;
    }
    let fields = &data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
    for (_, value) in fields.buffer.data.iter().rev() {
        ctx.charge(1)?;
        values.push(ctx, value.clone())?;
    }
    Ok(())
}

pub(crate) fn bindings(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
) -> Result<Buffer<(Value, Value)>> {
    let heap = instance.heap()?;
    let mut data = heap.data.lock().unwrap();
    let fields = &data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
    let mut values = Buffer::with_capacity(ctx, fields.buffer.data.len())?;
    values.extend(ctx, &fields.buffer.data)?;
    for (_, value) in &mut values.data {
        ctx.charge(1)?;
        if let Some(mapped) = data.map(ctx, &heap, value, false)? {
            *value = mapped;
        }
    }
    Ok(values)
}

pub(crate) fn set(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    name: &str,
    value: &Value,
) -> Result<()> {
    let heap = instance.writable_heap(ctx)?;
    let value = ctx.import(value)?;
    let key = ctx.bytes(name.as_bytes())?;
    let mut data = heap.data.lock().unwrap();
    let value = data.map(ctx, &heap, &value, true)?.unwrap_or(value);
    data.reserve_scratch(ctx, value.depth())?;
    data.entries.data[instance.identity.slot.load(Ordering::Relaxed)]
        .fields
        .insert_field(ctx, key, value)
}

pub(crate) fn address(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    name: &str,
) -> Result<crate::address::Address> {
    let heap = instance.writable_heap(ctx)?;
    let mut data = heap.data.lock().unwrap();
    let instance = data
        .instance(ctx, &heap, instance, false)?
        .unwrap_or_else(|| instance.clone());
    let object = instance.identity.slot.load(Ordering::Relaxed);
    let field = if let Some(field) = data.entries.data[object]
        .fields
        .find(ctx, name.as_bytes())?
    {
        field
    } else {
        let key = ctx.bytes(name.as_bytes())?;
        let field = data.entries.data[object].fields.buffer.data.len();
        data.entries.data[object]
            .fields
            .insert_field(ctx, key, Value::nil())?;
        field
    };
    let value = data.entries.data[object].fields.buffer.data[field]
        .1
        .clone();
    let value = data.map(ctx, &heap, &value, false)?.unwrap_or(value);
    Ok(crate::address::Address::object(
        instance.clone(),
        field,
        value,
    ))
}

pub(crate) fn field_name(instance: &Arc<Instance>, field: usize) -> Result<Value> {
    let heap = instance.heap()?;
    let data = heap.data.lock().unwrap();
    Ok(
        data.entries.data[instance.identity.slot.load(Ordering::Relaxed)]
            .fields
            .buffer
            .data[field]
            .0
            .clone(),
    )
}

pub(crate) fn field_slot(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    name: &str,
) -> Result<Option<usize>> {
    let heap = instance.heap()?;
    let data = heap.data.lock().unwrap();
    data.entries.data[instance.identity.slot.load(Ordering::Relaxed)]
        .fields
        .find(ctx, name.as_bytes())
}

pub(crate) fn set_slot(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    field: usize,
    value: Value,
) -> Result<()> {
    let heap = instance.writable_heap(ctx)?;
    let value = ctx.import(&value)?;
    let mut data = heap.data.lock().unwrap();
    let value = data.map(ctx, &heap, &value, true)?.unwrap_or(value);
    data.reserve_scratch(ctx, value.depth())?;
    let fields = &mut data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
    let name = fields.buffer.data[field].0.clone();
    fields.insert_field(ctx, name, value)
}

fn import_root(ctx: &mut CallContext, instance: &Arc<Instance>) -> Result<Arc<Instance>> {
    if ctx.snapshot_objects.is_some() {
        return snapshot_root(ctx, instance);
    }
    let source = instance.heap()?;
    if Arc::ptr_eq(&source.owner, &ctx.identity()) {
        return if matches!(instance.owner, Owner::External(_)) {
            Ok(instance.clone())
        } else {
            source
                .data
                .lock()
                .unwrap()
                .root(ctx, &source, &instance.identity)
        };
    }
    let target = local(ctx)?;
    {
        let data = target.data.lock().unwrap();
        let mut found = None;
        for (from, to) in &data.imports.data {
            ctx.charge(1)?;
            if *from == instance.identity.id && to.slot.load(Ordering::Relaxed) != usize::MAX {
                found = Some(to.clone());
                break;
            }
        }
        if let Some(identity) = found {
            return data.root(ctx, &target, &identity);
        }
    }
    let result = new_scoped(ctx, instance.class(), Some(instance.nominal_scope()))?;
    target
        .data
        .lock()
        .unwrap()
        .imports
        .push(ctx, (instance.identity.id, result.identity.clone()))?;
    queue_import(ctx, instance, result)
}

fn snapshot_root(ctx: &mut CallContext, instance: &Arc<Instance>) -> Result<Arc<Instance>> {
    let mut index = 0;
    while index < ctx.snapshot_objects.as_ref().unwrap().data.len() {
        ctx.charge(1)?;
        let (source, target) = &ctx.snapshot_objects.as_ref().unwrap().data[index];
        // Importing already copied fields and class environments must reuse the
        // new identities instead of making snapshots of the snapshot itself.
        if *source == instance.identity.id || target.same(instance) {
            return Ok(target.clone());
        }
        index += 1;
    }
    let result = new_scoped(ctx, instance.class(), Some(instance.nominal_scope()))?;
    let mut copies = ctx.snapshot_objects.take().unwrap();
    let saved = copies.push(ctx, (instance.identity.id, result.clone()));
    ctx.snapshot_objects = Some(copies);
    saved?;
    queue_import(ctx, instance, result)
}

fn queue_import(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    result: Arc<Instance>,
) -> Result<Arc<Instance>> {
    let mut pending = std::mem::replace(&mut ctx.pending_objects, Buffer::empty());
    let queued = pending.push(ctx, (instance.clone(), result.clone()));
    ctx.pending_objects = pending;
    queued?;
    Ok(result)
}

pub(crate) fn import(ctx: &mut CallContext, instance: &Arc<Instance>) -> Result<Arc<Instance>> {
    if ctx.importing_objects {
        return import_root(ctx, instance);
    }
    ctx.importing_objects = true;
    let work = (|| -> Result<Arc<Instance>> {
        let result = import_root(ctx, instance)?;
        while let Some((instance, target)) = ctx.pending_objects.data.pop() {
            ctx.charge(1)?;
            let fields = bindings(ctx, &instance)?;
            for (name, value) in fields.data {
                let value = ctx.import_rooted(&value)?;
                let name = std::str::from_utf8(name.as_bytes().unwrap())
                    .map_err(|_| Error::new(ErrorKind::Type, "instance field name is not UTF-8"))?;
                set(ctx, &target, name, &value)?;
            }
        }
        Ok(result)
    })();
    ctx.importing_objects = false;
    ctx.pending_objects = Buffer::empty();
    work
}

pub(crate) fn finish(ctx: &mut CallContext) -> Result<()> {
    if let Some(heap) = ctx.objects.take() {
        if Arc::strong_count(&heap) != 1 {
            if let Err(error) = collect(ctx, &heap, true) {
                ctx.objects = Some(heap);
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) fn cleanup(ctx: &mut CallContext) {
    ctx.pending_objects = Buffer::empty();
    if let Some(heap) = ctx.objects.take() {
        if Arc::strong_count(&heap) != 1 {
            let mut data = heap.data.lock().unwrap();
            let depth = reclaim(&mut data, &mut || Ok(())).ok();
            data.imports = Buffer::empty();
            data.pending = Buffer::empty();
            if depth == Some(0) {
                data.scratch = Buffer::empty();
            }
            prune_classes(&mut data);
            let limit = ctx.options.limits.memory_bytes;
            data.classes.shrink_after_failure(limit);
            data.entries.shrink_after_failure(limit);
            data.pending.shrink_after_failure(limit);
        }
    }
}

fn collect(ctx: &mut CallContext, heap: &Arc<Heap>, shrink: bool) -> Result<()> {
    let mut data = heap.data.lock().unwrap();
    let depth = reclaim(&mut data, &mut || ctx.charge(1))?;
    if depth == 0 {
        data.scratch = Buffer::empty();
    }
    let mut slot = 0;
    while slot < data.classes.data.len() {
        ctx.charge(1)?;
        let mut used = false;
        for entry in &data.entries.data {
            ctx.charge(1)?;
            if entry
                .internal
                .class()
                .same_binding(&data.classes.data[slot])
            {
                used = true;
                break;
            }
        }
        if used {
            slot += 1;
        } else {
            data.classes.data.swap_remove(slot);
        }
    }
    if shrink {
        data.imports = Buffer::empty();
        data.classes.shrink(ctx)?;
        data.entries.shrink(ctx)?;
        data.pending.shrink(ctx)?;
        // Retained instances can still be imported or collected after finish.
        // Their field traversal must remain possible after budget exhaustion.
        if data.entries.data.is_empty() {
            data.scratch = Buffer::empty();
        }
    }
    Ok(())
}

fn prune_classes(data: &mut Data) {
    let mut slot = 0;
    while slot < data.classes.data.len() {
        if data.entries.data.iter().any(|entry| {
            entry
                .internal
                .class()
                .same_binding(&data.classes.data[slot])
        }) {
            slot += 1;
        } else {
            data.classes.data.swap_remove(slot);
        }
    }
}

fn reclaim(data: &mut Data, tick: &mut impl FnMut() -> Result<()>) -> Result<usize> {
    let mut depth = 0;
    data.pending.data.clear();
    data.scratch.data.clear();
    for (slot, entry) in data.entries.data.iter().enumerate() {
        tick()?;
        let rooted = entry.internal.identity.roots.load(Ordering::Relaxed) != 0;
        entry
            .internal
            .identity
            .marked
            .store(rooted, Ordering::Relaxed);
        if rooted {
            data.pending.data.push(slot);
        }
    }
    while let Some(slot) = data.pending.data.pop() {
        if let Some(environment) = &data.entries.data[slot].internal.class().environment {
            tick()?;
            mark_instance(environment, &mut data.pending.data);
        }
        for (_, value) in &data.entries.data[slot].fields.buffer.data {
            depth = depth.max(value.depth());
            mark_references(value, &mut data.pending.data, &mut data.scratch.data, tick)?;
        }
    }
    let mut slot = 0;
    while slot < data.entries.data.len() {
        tick()?;
        if data.entries.data[slot]
            .internal
            .identity
            .marked
            .load(Ordering::Relaxed)
        {
            slot += 1;
        } else {
            let removed = data.entries.data.swap_remove(slot);
            removed
                .internal
                .identity
                .slot
                .store(usize::MAX, Ordering::Relaxed);
            if let Some(moved) = data.entries.data.get(slot) {
                moved.internal.identity.slot.store(slot, Ordering::Relaxed);
            }
        }
    }
    data.imports
        .data
        .retain(|(_, to)| to.slot.load(Ordering::Relaxed) != usize::MAX);
    data.allocations = 0;
    Ok(depth)
}

fn mark_instance(instance: &Arc<Instance>, pending: &mut Vec<usize>) {
    if !instance.identity.marked.swap(true, Ordering::Relaxed) {
        debug_assert!(pending.len() < pending.capacity());
        pending.push(instance.identity.slot.load(Ordering::Relaxed));
    }
}

/// Marks every instance reachable from one field value. Container frames use
/// the heap's pre-reserved scratch so marking allocates nothing, which keeps
/// it usable during cleanup after the budget is exhausted.
fn mark_references(
    value: &Value,
    pending: &mut Vec<usize>,
    scratch: &mut Vec<(Value, usize)>,
    tick: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    debug_assert!(scratch.is_empty());
    let result = mark_walk(value, pending, scratch, tick);
    scratch.clear();
    result
}

fn mark_walk(
    value: &Value,
    pending: &mut Vec<usize>,
    scratch: &mut Vec<(Value, usize)>,
    tick: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    if !mark_value(value, pending, tick, 0)? {
        return Ok(());
    }
    push_scratch(scratch, value.clone())?;
    loop {
        let depth = scratch.len();
        let Some((container, position)) = scratch.last_mut() else {
            return Ok(());
        };
        let child = match &container.0 {
            Kind::Array(array) => array.buffer.data.get(*position),
            Kind::Hash(hash) => hash.buffer.data.get(*position).map(|(_, value)| value),
            _ => unreachable!(),
        };
        let Some(child) = child else {
            scratch.pop();
            continue;
        };
        *position += 1;
        let next = if mark_value(child, pending, tick, depth)? {
            Some(child.clone())
        } else {
            None
        };
        if let Some(next) = next {
            push_scratch(scratch, next)?;
        }
    }
}

/// Marks one value and reports whether it is a container to descend into.
fn mark_value(
    value: &Value,
    pending: &mut Vec<usize>,
    tick: &mut impl FnMut() -> Result<()>,
    depth: usize,
) -> Result<bool> {
    tick()?;
    if depth > MAX_VALUE_DEPTH {
        return Err(Error::limit(
            ErrorKind::Recursion,
            "instance field nesting too deep",
        ));
    }
    match &value.0 {
        Kind::Function(function) => mark_instance(&function.environment, pending),
        Kind::Instance(instance) => mark_instance(instance, pending),
        Kind::Namespace(namespace) => {
            if let Some(environment) = &namespace.environment {
                tick()?;
                mark_instance(environment, pending);
            }
        }
        Kind::Array(_) | Kind::Hash(_) => return Ok(true),
        _ => (),
    }
    Ok(false)
}

fn push_scratch(scratch: &mut Vec<(Value, usize)>, value: Value) -> Result<()> {
    // Field writes reserve one frame per nesting level, so this never allocates.
    debug_assert!(scratch.len() < scratch.capacity());
    if scratch.len() == scratch.capacity() {
        return Err(Error::limit(
            ErrorKind::Recursion,
            "instance field nesting too deep",
        ));
    }
    scratch.push((value, 0));
    Ok(())
}

#[cfg(test)]
mod retirement_tests;

#[cfg(test)]
mod environments_tests;

#[cfg(test)]
mod import_tests;

#[cfg(test)]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, namespace::Definition};

    fn class() -> Arc<Namespace> {
        Namespace::untracked(Definition::new(
            0,
            "Node".into(),
            Vec::new(),
            Vec::new(),
            Some((0, false)),
            Vec::new(),
            None,
        ))
    }

    #[test]
    fn foreign_writes_require_an_independent_import() {
        let mut source = CallContext::new(CallOptions::default());
        let root = new(&mut source, &class()).unwrap();
        set(&mut source, &root, "value", &Value::int(1)).unwrap();
        let mut target = CallContext::new(CallOptions::default());
        assert_eq!(
            set(&mut target, &root, "value", &Value::int(2))
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
        assert!(matches!(
            address(&mut target, &root, "missing"),
            Err(Error {
                kind: ErrorKind::Type,
                ..
            })
        ));
        assert_eq!(
            set_slot(&mut target, &root, 0, Value::int(3))
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
        assert_eq!(target.stats().retained_memory_bytes, 0);
        assert_eq!(
            field(&mut source, &root, "value")
                .unwrap()
                .unwrap()
                .as_int(),
            Some(1)
        );
        assert!(field(&mut source, &root, "missing").unwrap().is_none());
        drop(root);
        finish(&mut source).unwrap();
        assert_eq!(source.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn imports_do_not_retain_the_source_invocation() {
        let class = class();
        let mut source = CallContext::new(CallOptions::default());
        let root = new(&mut source, &class).unwrap();
        set(
            &mut source,
            &root,
            "link",
            &Value(Kind::Instance(root.clone())),
        )
        .unwrap();
        finish(&mut source).unwrap();
        let mut target = CallContext::new(CallOptions::default());
        let copied = import(&mut target, &root).unwrap();
        drop(root);
        assert_eq!(source.stats().retained_memory_bytes, 0);
        let value = field(&mut target, &copied, "link").unwrap().unwrap();
        assert!(matches!(&value.0, Kind::Instance(link) if copied.same(link)));
        drop(value);
        finish(&mut target).unwrap();
        drop(copied);
        assert_eq!(target.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn promoting_fields_does_not_leave_weak_handle_allocations() {
        let class = class();
        let mut ctx = CallContext::new(CallOptions::default());
        let a = new(&mut ctx, &class).unwrap();
        let b = new(&mut ctx, &class).unwrap();
        set(&mut ctx, &a, "link", &Value(Kind::Instance(b.clone()))).unwrap();
        drop(b);
        let baseline = ctx.stats().retained_memory_bytes;
        for _ in 0..1000 {
            let child = field(&mut ctx, &a, "link").unwrap().unwrap();
            drop(child);
            assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        }
        drop(a);
        finish(&mut ctx).unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn failure_cleanup_reclaims_unrelated_graphs_and_keeps_exhaustion_latched() {
        let class = class();
        let mut ctx = CallContext::new(CallOptions::default());
        let kept = new(&mut ctx, &class).unwrap();
        let mut roots = Buffer::with_capacity(&mut ctx, 2048).unwrap();
        let bytes = vec![b'x'; 4096];
        for _ in 0..2048 {
            let node = new(&mut ctx, &class).unwrap();
            let bytes = ctx.bytes(&bytes).unwrap();
            set(&mut ctx, &node, "payload", &bytes).unwrap();
            roots.data.push(node);
        }
        assert!(ctx.stats().retained_memory_bytes > 8 << 20);
        ctx.options.limits.steps = Some(ctx.stats().steps);
        assert_eq!(ctx.charge(1).unwrap_err().kind, ErrorKind::Steps);
        drop(roots);
        cleanup(&mut ctx);
        assert!(
            ctx.stats().retained_memory_bytes < 8192,
            "{:?}",
            ctx.stats()
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
        drop(kept);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
