//! Runs a program's recorded invocations and gathers the types that flow
//! through each site.

use super::types::{Typer, Types};
use serde_json::Value as Json;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use vibescript::{
    CallOptions, Engine, Limits, ModuleConfig, Value,
    observe::{Event, Observer, Site},
};

/// One recorded call of a program: its entry point, arguments and the host
/// configuration it ran with.
///
/// Inputs use the golden corpora's case fields: `function`, `args` (plain
/// JSON), `typed_args`, `typed_kwargs` and `typed_globals` (typed-v1 nodes),
/// `globals`, `module_paths` (relative to the inputs file), `module_allow`,
/// `module_deny`, `module_development`, `allow_require`, `strict_effects`,
/// `entropy_byte`, `stdout`, `stderr`, `steps`, `memory` and `recursion`. An
/// optional `file` names the source it applies to, relative to the directory
/// being migrated; without it the invocation applies to every file.
#[derive(Clone, Debug)]
pub struct Invocation {
    pub file: Option<String>,
    fields: Json,
    base: PathBuf,
}

impl Invocation {
    /// Reads one invocation from a JSON object whose relative module paths
    /// resolve against `base`.
    pub fn from_json(fields: Json, base: &Path) -> Result<Self, String> {
        if !fields.is_object() {
            return Err("an invocation must be a JSON object".to_owned());
        }
        let file = match fields.get("file") {
            None | Some(Json::Null) => None,
            Some(Json::String(file)) => Some(file.clone()),
            Some(_) => return Err("an invocation's file must be a string".to_owned()),
        };
        Ok(Self {
            file,
            fields,
            base: base.to_owned(),
        })
    }

    /// Reads a JSON Lines file of invocations, one object per line.
    pub fn read(path: &Path) -> Result<Vec<Self>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        text.lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                let fields = serde_json::from_str(line)
                    .map_err(|e| format!("{}:{}: {e}", path.display(), index + 1))?;
                Self::from_json(fields, base)
                    .map_err(|e| format!("{}:{}: {e}", path.display(), index + 1))
            })
            .collect()
    }

    fn function(&self) -> Option<&str> {
        self.fields.get("function").and_then(Json::as_str)
    }

    fn flag(&self, name: &str) -> bool {
        self.fields
            .get(name)
            .and_then(Json::as_bool)
            .unwrap_or(false)
    }

    fn strings(&self, name: &str) -> Vec<String> {
        self.fields
            .get(name)
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    }
}

/// Types by a key and a name, such as a function's offset and a parameter's
/// name, looked up without allocating.
#[derive(Debug)]
pub(crate) struct Keyed<K, V = Types>(HashMap<K, Vec<(String, V)>>);

impl<K, V> Default for Keyed<K, V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Default> Keyed<K, V> {
    pub fn get(&self, key: &K, name: &str) -> Option<&V> {
        self.0
            .get(key)?
            .iter()
            .find(|(own, _)| own == name)
            .map(|(_, value)| value)
    }

    pub fn entry(&mut self, key: K, name: &str) -> &mut V {
        let names = self.0.entry(key).or_default();
        let index = match names.iter().position(|(own, _)| own == name) {
            Some(index) => index,
            None => {
                names.push((name.to_owned(), V::default()));
                names.len() - 1
            }
        };
        &mut names[index].1
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &str, &V)> {
        self.0.iter().flat_map(|(key, names)| {
            names
                .iter()
                .map(move |(name, value)| (key, name.as_str(), value))
        })
    }

    fn merge_with(&mut self, other: Self, mut merge: impl FnMut(&mut V, V)) {
        for (key, names) in other.0 {
            for (name, value) in names {
                merge(self.entry(key.clone(), &name), value);
            }
        }
    }
}

/// The types observed at each site of one source, keyed by the offsets the
/// compiler reports.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    /// Parameters by function offset and name.
    pub params: Keyed<usize>,
    /// Results by function offset.
    pub returns: HashMap<usize, Types>,
    /// Functions whose parameters bound, by offset.
    pub entered: HashMap<usize, usize>,
    /// Functions that started, before their arguments bound, by offset.
    pub starts: HashMap<usize, usize>,
    pub block_results: HashMap<usize, Types>,
    /// Each yield's arguments, by position.
    pub yields: HashMap<usize, Vec<Types>>,
    pub yield_results: HashMap<usize, Types>,
    /// Local writes and reads, by offset and name.
    pub stores: Keyed<usize>,
    pub loads: Keyed<usize>,
    /// Instance variables by class and name.
    pub instance: Keyed<String>,
    /// Member receivers by offset and member name.
    pub receivers: Keyed<usize>,
    /// How often each member call started and returned, by offset and name.
    pub calls: Keyed<usize, (usize, usize)>,
    /// What member calls returned, by offset and name.
    pub results: Keyed<usize>,
    /// Index receivers and their selectors, by offset.
    pub indexes: HashMap<usize, (Types, Types)>,
    /// Binary operators by offset: the operand types and how often both were integers.
    pub binaries: HashMap<usize, Binary>,
    pub unaries: HashMap<usize, Types>,
    /// Conditions by the offset of their branch and of the operation that
    /// computed the value.
    pub conditions: HashMap<(usize, usize), Types>,
    /// Case subjects by offset.
    pub cases: HashMap<usize, Types>,
}

#[derive(Debug, Default)]
pub(crate) struct Binary {
    pub left: Types,
    pub right: Types,
    /// How often both operands were integers, and how often they were not.
    pub integers: usize,
    pub others: usize,
}

/// Facts for every source the recorded runs executed, by source text.
#[derive(Debug, Default)]
pub struct Observations {
    sources: HashMap<String, Facts>,
    /// Sources some recorded run of could not be observed, so what the other
    /// runs saw may not cover what the program does.
    untrusted: std::collections::HashSet<String>,
    /// Invocations that could not run, with why.
    pub failures: Vec<String>,
}

impl Observations {
    pub(crate) fn facts(&self, source: &str) -> Option<&Facts> {
        if self.untrusted.contains(source) {
            return None;
        }
        self.sources.get(source)
    }

    /// Ignores what runs observed of `source`, for a program some recorded
    /// run of could not be reproduced, such as one that needs host capabilities.
    pub fn distrust(&mut self, source: &str) {
        self.untrusted.insert(source.to_owned());
    }

    /// Whether any run executed `source`.
    pub fn observed(&self, source: &str) -> bool {
        self.sources.contains_key(source)
    }

    /// Adds every fact of `other`.
    pub fn merge(&mut self, other: Observations) {
        for (source, facts) in other.sources {
            match self.sources.get_mut(&source) {
                Some(mine) => mine.merge(facts),
                None => {
                    self.sources.insert(source, facts);
                }
            }
        }
        self.untrusted.extend(other.untrusted);
        self.failures.extend(other.failures);
    }
}

fn merge_map<K: std::hash::Hash + Eq>(mine: &mut HashMap<K, Types>, other: HashMap<K, Types>) {
    for (key, types) in other {
        mine.entry(key).or_default().join(&types);
    }
}

impl Facts {
    fn merge(&mut self, other: Facts) {
        let join = |mine: &mut Types, other: Types| mine.join(&other);
        self.params.merge_with(other.params, join);
        merge_map(&mut self.returns, other.returns);
        for (key, count) in other.entered {
            *self.entered.entry(key).or_default() += count;
        }
        for (key, count) in other.starts {
            *self.starts.entry(key).or_default() += count;
        }
        merge_map(&mut self.block_results, other.block_results);
        for (key, args) in other.yields {
            let mine = self.yields.entry(key).or_default();
            if mine.len() < args.len() {
                mine.resize(args.len(), Types::default());
            }
            for (slot, types) in mine.iter_mut().zip(&args) {
                slot.join(types);
            }
        }
        merge_map(&mut self.yield_results, other.yield_results);
        self.stores.merge_with(other.stores, join);
        self.loads.merge_with(other.loads, join);
        self.instance.merge_with(other.instance, join);
        self.receivers.merge_with(other.receivers, join);
        self.results.merge_with(other.results, join);
        self.calls
            .merge_with(other.calls, |mine, (started, returned)| {
                mine.0 += started;
                mine.1 += returned;
            });
        for (key, (receiver, keys)) in other.indexes {
            let mine = self.indexes.entry(key).or_default();
            mine.0.join(&receiver);
            mine.1.join(&keys);
        }
        for (key, binary) in other.binaries {
            let mine = self.binaries.entry(key).or_default();
            mine.left.join(&binary.left);
            mine.right.join(&binary.right);
            mine.integers += binary.integers;
            mine.others += binary.others;
        }
        merge_map(&mut self.unaries, other.unaries);
        merge_map(&mut self.conditions, other.conditions);
        merge_map(&mut self.cases, other.cases);
    }

    fn record(&mut self, event: &Event<'_>, typer: &mut Typer) {
        let offset = event.offset;
        let values = event.values;
        let mut add = |types: &mut Types, values: &[Value]| {
            for value in values {
                typer.add(types, value);
            }
        };
        match &event.site {
            Site::Enter { .. } => *self.starts.entry(offset).or_default() += 1,
            Site::Parameter { name, index, .. } => {
                add(self.params.entry(offset, name), values);
                if *index == 0 {
                    *self.entered.entry(offset).or_default() += 1;
                }
            }
            Site::Return { .. } => add(self.returns.entry(offset).or_default(), values),
            Site::BlockResult => add(self.block_results.entry(offset).or_default(), values),
            Site::Yield => {
                let slots = self.yields.entry(offset).or_default();
                if slots.len() < values.len() {
                    slots.resize(values.len(), Types::default());
                }
                for (slot, value) in slots.iter_mut().zip(values) {
                    add(slot, std::slice::from_ref(value));
                }
            }
            Site::YieldResult => add(self.yield_results.entry(offset).or_default(), values),
            Site::Store { name } => add(self.stores.entry(offset, name), values),
            Site::Load { name } => add(self.loads.entry(offset, name), values),
            Site::InstanceStore { name } | Site::InstanceLoad { name } => {
                if let [instance, value] = values
                    && let Some(class) = vibescript::observe::class_name(instance)
                {
                    let types = self.instance.entry(class.replace("::", "."), name);
                    add(types, std::slice::from_ref(value));
                }
            }
            Site::Receiver { member } => {
                add(self.receivers.entry(offset, member), values);
                self.calls.entry(offset, member).0 += 1;
            }
            Site::Result { member } => {
                self.calls.entry(offset, member).1 += 1;
                add(self.results.entry(offset, member), values);
            }
            Site::Index => {
                if let [receiver, keys @ ..] = values {
                    let entry = self.indexes.entry(offset).or_default();
                    add(&mut entry.0, std::slice::from_ref(receiver));
                    add(&mut entry.1, keys);
                }
            }
            Site::Binary { .. } => {
                if let [left, right] = values {
                    let binary = self.binaries.entry(offset).or_default();
                    add(&mut binary.left, std::slice::from_ref(left));
                    add(&mut binary.right, std::slice::from_ref(right));
                    let integer = |value: &Value| value.type_name() == "int";
                    if integer(left) && integer(right) {
                        binary.integers += 1;
                    } else {
                        binary.others += 1;
                    }
                }
            }
            Site::Unary { .. } => add(self.unaries.entry(offset).or_default(), values),
            Site::Condition { origin } => add(
                self.conditions.entry((offset, *origin)).or_default(),
                values,
            ),
            Site::Case => {
                if let [subject, _] = values {
                    add(
                        self.cases.entry(offset).or_default(),
                        std::slice::from_ref(subject),
                    );
                }
            }
            _ => (),
        }
    }
}

/// Collects events per source text.
#[derive(Default)]
struct Collector {
    state: Mutex<(HashMap<String, Facts>, Typer)>,
}

impl Observer for Collector {
    fn observe(&self, event: &Event<'_>) {
        let mut state = self.state.lock().unwrap();
        let (sources, typer) = &mut *state;
        if !sources.contains_key(event.source) {
            sources.insert(event.source.to_owned(), Facts::default());
        }
        sources.get_mut(event.source).unwrap().record(event, typer);
    }
}

/// Case fields that need host capabilities this runner does not provide.
const UNSUPPORTED: [&str; 4] = [
    "capability_probe",
    "block_probe",
    "signature_probe",
    "notifications",
];

impl Invocation {
    /// Whether this invocation needs host capabilities that observation
    /// cannot provide, so its run cannot be reproduced.
    pub fn reproducible(&self) -> bool {
        UNSUPPORTED.iter().all(|field| {
            self.fields
                .get(*field)
                .is_none_or(|value| value.is_null() || *value == Json::Bool(false))
        })
    }

    /// The directories this invocation loads required files from.
    pub fn module_paths(&self) -> Vec<PathBuf> {
        self.strings("module_paths")
            .into_iter()
            .map(|path| self.base.join(path))
            .collect()
    }
}

/// Runs `invocations` of `source` and returns what they observed. Runs that
/// fail still contribute what they observed before failing. A source with
/// an invocation that cannot be reproduced is not trusted at all, since its
/// other runs may miss what that one does.
pub fn observe(source: &str, invocations: &[Invocation]) -> Observations {
    let collector = Arc::new(Collector::default());
    let mut failures = Vec::new();
    let mut untrusted = std::collections::HashSet::new();
    for invocation in invocations {
        if !invocation.reproducible() {
            untrusted.insert(source.to_owned());
            continue;
        }
        if let Err(message) = run(source, invocation, collector.clone()) {
            failures.push(message);
        }
    }
    let collector = Arc::try_unwrap(collector).unwrap_or_default();
    let (mut sources, _) = collector.state.into_inner().unwrap();
    // A program that ran records its entry even when it had no events.
    sources.entry(source.to_owned()).or_default();
    Observations {
        sources,
        untrusted,
        failures,
    }
}

fn run(source: &str, invocation: &Invocation, collector: Arc<Collector>) -> Result<(), String> {
    let mut engine = Engine::new();
    engine.set_strict_effects(invocation.flag("strict_effects"));
    let paths = invocation.strings("module_paths");
    if !paths.is_empty() {
        let config = ModuleConfig {
            paths: paths
                .iter()
                .map(|path| invocation.base.join(path))
                .collect(),
            allow: invocation.strings("module_allow"),
            deny: invocation.strings("module_deny"),
            development: invocation.flag("module_development"),
            ..ModuleConfig::default()
        };
        engine
            .set_module_config(config)
            .map_err(|e| e.to_string())?;
    }
    if let Some(byte) = invocation.fields.get("entropy_byte").and_then(Json::as_u64) {
        let byte = byte as u8;
        engine.set_random_source(move |_, output| {
            output.fill(byte);
            Ok(output.len())
        });
    } else {
        let state = AtomicU64::new(0);
        engine.set_random_source(move |_, output| {
            for chunk in output.chunks_mut(8) {
                let seed = state.fetch_add(1, Ordering::Relaxed);
                chunk.copy_from_slice(&splitmix(seed).to_le_bytes()[..chunk.len()]);
            }
            Ok(output.len())
        });
    }
    // Without a writer, output helpers raise, and some recorded runs rely on that.
    if invocation.fields.get("stdout").and_then(Json::as_bool) != Some(false) {
        engine.set_output_writer(|_, _| Ok(()));
    }
    if invocation.fields.get("stderr").and_then(Json::as_bool) != Some(false) {
        engine.set_error_writer(|_, _| Ok(()));
    }
    engine.set_observer(collector);
    let script = engine.compile(source).map_err(|e| e.to_string())?;
    let Some(function) = invocation.function() else {
        return Ok(());
    };
    let (options, args, keywords) = inputs(invocation)?;
    // Errors are part of what a run does; the facts before them stand.
    let _ = script.call_with_keywords(function, &args, &keywords, options);
    Ok(())
}

type Inputs = (CallOptions, Vec<Value>, Vec<(String, Value)>);

fn inputs(invocation: &Invocation) -> Result<Inputs, String> {
    let fields = &invocation.fields;
    let mut args = Vec::new();
    for arg in fields
        .get("args")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        args.push(plain(arg)?);
    }
    for node in fields
        .get("typed_args")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        args.push(decode(node)?);
    }
    let mut keywords = Vec::new();
    for pair in fields
        .get("typed_kwargs")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        keywords.push((
            pair[0].as_str().unwrap_or_default().to_owned(),
            decode(&pair[1])?,
        ));
    }
    let mut globals = BTreeMap::new();
    for (name, value) in fields
        .get("globals")
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
    {
        globals.insert(name.clone(), plain(value)?);
    }
    for pair in fields
        .get("typed_globals")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        globals.insert(
            pair[0].as_str().unwrap_or_default().to_owned(),
            decode(&pair[1])?,
        );
    }
    // Unmetered runs have no limits; metered ones get at least the defaults.
    let metered = fields
        .get("accounting")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    let limit = |name: &str, default: u64| match fields.get(name) {
        Some(Json::Null) => None,
        Some(value) => Some(value.as_u64().unwrap_or(default).max(default)),
        None => metered.then_some(default),
    };
    let options = CallOptions {
        globals,
        allow_require: invocation.flag("allow_require"),
        limits: Limits {
            steps: limit("steps", 5_000_000),
            memory_bytes: limit("memory", 64 << 20).map(|bytes| bytes as usize),
            recursion: fields
                .get("recursion")
                .and_then(Json::as_u64)
                .map_or(256, |depth| depth as usize),
        },
        deadline: Some(Instant::now() + Duration::from_secs(60)),
        ..CallOptions::default()
    };
    Ok((options, args, keywords))
}

fn plain(value: &Json) -> Result<Value, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    vibescript::parse_json(&bytes, CallOptions::default())
        .map(|outcome| outcome.value)
        .map_err(|e| e.to_string())
}

/// Decodes a typed-v1 node, as the golden corpora record values.
fn decode(node: &Json) -> Result<Value, String> {
    let kind = node[0].as_str().ok_or("typed node without a kind")?;
    let text = |index: usize| {
        node[index]
            .as_str()
            .ok_or_else(|| format!("malformed {kind} node"))
    };
    let number = |index: usize| {
        text(index)?
            .parse::<i64>()
            .map_err(|e| format!("{kind}: {e}"))
    };
    Ok(match kind {
        "nil" => Value::nil(),
        "bool" => Value::boolean(node[1].as_bool().ok_or("malformed bool node")?),
        "int" => match text(1)?.parse::<i64>() {
            Ok(value) => Value::int(value),
            Err(_) => Value::parse_integer(text(1)?, 10).map_err(|e| e.to_string())?,
        },
        "float" => Value::float(f64::from_bits(
            u64::from_str_radix(text(1)?, 16).map_err(|e| e.to_string())?,
        )),
        "string" => Value::bytes(unhex(text(1)?)?),
        "symbol" => Value::symbol(unhex(text(1)?)?),
        "money" => Value::money(number(2)?, text(1)?).map_err(|e| e.to_string())?,
        "duration" => Value::duration(number(1)?),
        "time" => {
            let nanos = text(2)?.parse::<u32>().map_err(|e| e.to_string())?;
            Value::time(number(1)?, nanos).map_err(|e| e.to_string())?
        }
        "range" => {
            let bound = |index: usize| match &node[index] {
                Json::Null => Ok(None),
                Json::String(text) => text.parse::<i64>().map(Some).map_err(|e| e.to_string()),
                _ => Err("malformed range node".to_owned()),
            };
            Value::range(bound(1)?, bound(2)?, node[3].as_bool().unwrap_or(false))
        }
        "array" => Value::array(
            node[1]
                .as_array()
                .ok_or("malformed array node")?
                .iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
        ),
        "hash" | "object" => {
            let mut entries = Vec::new();
            for entry in node[1].as_array().ok_or("malformed hash node")? {
                let key = unhex(entry[0].as_str().ok_or("malformed hash key")?)?;
                entries.push((key, decode(&entry[1])?));
            }
            if kind == "hash" {
                Value::hash(entries)
            } else {
                Value::object(entries)
            }
        }
        other => return Err(format!("unsupported typed node {other}")),
    })
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// The SplitMix64 output for one counter value, as the golden harness seeds entropy.
fn splitmix(counter: u64) -> u64 {
    let mut z = counter.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
