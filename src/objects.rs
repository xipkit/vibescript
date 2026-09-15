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
}

struct Entry {
    internal: Arc<Instance>,
    fields: Hash,
}

struct Identity {
    id: u64,
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
    pub fn class(&self) -> &Arc<Namespace> {
        &self.identity.class
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
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
        }),
        _header: header,
    });
    ctx.objects = Some(heap.clone());
    Ok(heap)
}

pub(crate) fn new(ctx: &mut CallContext, class: &Arc<Namespace>) -> Result<Arc<Instance>> {
    let heap = local(ctx)?;
    if heap.data.lock().unwrap().allocations >= 32 {
        collect(ctx, &heap, false)?;
    }
    let class = {
        let mut data = heap.data.lock().unwrap();
        let mut known = None;
        for candidate in &data.classes.data {
            ctx.charge(1)?;
            if Arc::ptr_eq(&candidate.definition, &class.definition) {
                known = Some(candidate.clone());
                break;
            }
        }
        if let Some(known) = known {
            known
        } else {
            let class = Namespace::import(ctx, class)?;
            data.classes.push(ctx, class.clone())?;
            class
        }
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

    fn map(
        &mut self,
        ctx: &mut CallContext,
        heap: &Arc<Heap>,
        value: &Value,
        internal: bool,
        depth: usize,
    ) -> Result<Option<Value>> {
        ctx.charge(1)?;
        if depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "instance field nesting too deep");
        }
        match &value.0 {
            Kind::Instance(instance) => {
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
                        Ok(Some(Value(Kind::Instance(
                            self.entries.data[slot].internal.clone(),
                        ))))
                    }
                    (_, false) => self
                        .root(ctx, heap, &instance.identity)
                        .map(|root| Some(Value(Kind::Instance(root)))),
                }
            }
            Kind::Array(array) => {
                let mut mapped: Option<Buffer<Value>> = None;
                for (i, value) in array.buffer.data.iter().enumerate() {
                    if let Some(value) = self.map(ctx, heap, value, internal, depth + 1)? {
                        if mapped.is_none() {
                            let mut buffer = Buffer::with_capacity(ctx, array.buffer.data.len())?;
                            buffer.extend(ctx, &array.buffer.data)?;
                            mapped = Some(buffer);
                        }
                        mapped.as_mut().unwrap().data[i] = value;
                    }
                }
                mapped
                    .map(|buffer| Value::from_array(ctx, buffer))
                    .transpose()
            }
            Kind::Hash(hash) => {
                let mut mapped: Option<Buffer<(Value, Value)>> = None;
                for (i, (_, value)) in hash.buffer.data.iter().enumerate() {
                    if let Some(value) = self.map(ctx, heap, value, internal, depth + 1)? {
                        if mapped.is_none() {
                            let mut buffer = Buffer::with_capacity(ctx, hash.buffer.data.len())?;
                            buffer.extend(ctx, &hash.buffer.data)?;
                            mapped = Some(buffer);
                        }
                        mapped.as_mut().unwrap().data[i].1 = value;
                    }
                }
                mapped
                    .map(|buffer| {
                        let mut mapped = Hash::from_entries(ctx, buffer)?;
                        mapped.object = hash.object;
                        mapped.tag = hash.tag;
                        Value::from_hash(ctx, mapped)
                    })
                    .transpose()
            }
            _ => Ok(None),
        }
    }
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
    Ok(Some(
        data.map(ctx, &heap, &value, false, 1)?.unwrap_or(value),
    ))
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
    let value = data.map(ctx, &heap, &value, true, 1)?.unwrap_or(value);
    data.entries.data[instance.identity.slot.load(Ordering::Relaxed)]
        .fields
        .insert(ctx, key, value)
}

pub(crate) fn address(
    ctx: &mut CallContext,
    instance: &Arc<Instance>,
    name: &str,
) -> Result<crate::address::Address> {
    let heap = instance.writable_heap(ctx)?;
    let mut data = heap.data.lock().unwrap();
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
            .insert(ctx, key, Value::nil())?;
        field
    };
    let value = data.entries.data[object].fields.buffer.data[field]
        .1
        .clone();
    let value = data.map(ctx, &heap, &value, false, 1)?.unwrap_or(value);
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
    let value = data.map(ctx, &heap, &value, true, 1)?.unwrap_or(value);
    let fields = &mut data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
    let name = fields.buffer.data[field].0.clone();
    fields.insert(ctx, name, value)
}

fn import_root(ctx: &mut CallContext, instance: &Arc<Instance>) -> Result<Arc<Instance>> {
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
    let result = new(ctx, instance.class())?;
    target
        .data
        .lock()
        .unwrap()
        .imports
        .push(ctx, (instance.identity.id, result.identity.clone()))?;
    let mut pending = std::mem::replace(&mut ctx.pending_objects, Buffer::empty());
    let queued = pending.push(ctx, (instance.clone(), result.clone()));
    ctx.pending_objects = pending;
    queued?;
    Ok(result)
}

pub(crate) fn import(ctx: &mut CallContext, instance: &Arc<Instance>) -> Result<Arc<Instance>> {
    let result = import_root(ctx, instance)?;
    if ctx.importing_objects {
        return Ok(result);
    }
    ctx.importing_objects = true;
    let work = (|| -> Result<()> {
        while let Some((instance, target)) = ctx.pending_objects.data.pop() {
            ctx.charge(1)?;
            let source = instance.heap()?;
            let fields = {
                let mut data = source.data.lock().unwrap();
                let entries = &data.entries.data[instance.identity.slot.load(Ordering::Relaxed)]
                    .fields
                    .buffer
                    .data;
                let mut fields = Buffer::with_capacity(ctx, entries.len())?;
                fields.extend(ctx, entries)?;
                for (_, value) in &mut fields.data {
                    *value = data
                        .map(ctx, &source, value, false, 1)?
                        .unwrap_or_else(|| value.clone());
                }
                fields
            };
            for (name, value) in fields.data {
                let value = ctx.import(&value)?;
                let name = std::str::from_utf8(name.as_bytes().unwrap())
                    .map_err(|_| Error::new(ErrorKind::Type, "instance field name is not UTF-8"))?;
                set(ctx, &target, name, &value)?;
            }
        }
        Ok(())
    })();
    ctx.importing_objects = false;
    ctx.pending_objects = Buffer::empty();
    work?;
    Ok(result)
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
            let _ = reclaim(&mut data, &mut || Ok(()));
            data.imports = Buffer::empty();
            data.pending = Buffer::empty();
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
    reclaim(&mut data, &mut || ctx.charge(1))?;
    if shrink {
        data.imports = Buffer::empty();
        let mut slot = 0;
        while slot < data.classes.data.len() {
            let mut used = false;
            for entry in &data.entries.data {
                ctx.charge(1)?;
                if Arc::ptr_eq(
                    &entry.internal.class().definition,
                    &data.classes.data[slot].definition,
                ) {
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
        data.classes.shrink(ctx)?;
        data.entries.shrink(ctx)?;
        data.pending.shrink(ctx)?;
    }
    Ok(())
}

fn prune_classes(data: &mut Data) {
    let mut slot = 0;
    while slot < data.classes.data.len() {
        if data.entries.data.iter().any(|entry| {
            Arc::ptr_eq(
                &entry.internal.class().definition,
                &data.classes.data[slot].definition,
            )
        }) {
            slot += 1;
        } else {
            data.classes.data.swap_remove(slot);
        }
    }
}

fn reclaim(data: &mut Data, tick: &mut impl FnMut() -> Result<()>) -> Result<()> {
    data.pending.data.clear();
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
        for (_, value) in &data.entries.data[slot].fields.buffer.data {
            mark_references(value, &mut data.pending.data, tick, 1)?;
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
    Ok(())
}

fn mark_references(
    value: &Value,
    pending: &mut Vec<usize>,
    tick: &mut impl FnMut() -> Result<()>,
    depth: usize,
) -> Result<()> {
    tick()?;
    if depth > MAX_VALUE_DEPTH {
        return Err(Error::limit(
            ErrorKind::Recursion,
            "instance field nesting too deep",
        ));
    }
    match &value.0 {
        Kind::Instance(instance) => {
            if !instance.identity.marked.swap(true, Ordering::Relaxed) {
                debug_assert!(pending.len() < pending.capacity());
                pending.push(instance.identity.slot.load(Ordering::Relaxed));
            }
        }
        Kind::Array(array) => {
            for value in &array.buffer.data {
                mark_references(value, pending, tick, depth + 1)?;
            }
        }
        Kind::Hash(hash) => {
            for (_, value) in &hash.buffer.data {
                mark_references(value, pending, tick, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod retirement_tests;

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
