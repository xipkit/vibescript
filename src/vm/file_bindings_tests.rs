use super::*;
use crate::{CallOptions, CancellationToken, Engine, Limits, Script, ScriptInner};
use std::sync::atomic::{AtomicUsize, Ordering};

fn file(engine: &Engine, source: &str) -> Script {
    Script {
        inner: Arc::new(ScriptInner {
            code: crate::code::Code::compile_file(source, &engine.hosts).unwrap(),
            loader: engine.loader.clone(),
            strict_effects: engine.strict_effects,
            random_source: engine.random_source.clone(),
            output_writer: engine.output_writer.clone(),
            error_writer: engine.error_writer.clone(),
        }),
    }
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = crate::stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

/// A module directory, removed when dropped. A typed script cannot call a
/// module that another script returns, so these tests require the files,
/// which gives each call a file environment of its own, and call the
/// functions they export.
struct Files(std::path::PathBuf);

impl Files {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".cache/tmp")
            .join(format!(
                "file-bindings-{}-{}",
                crate::loading::test_support::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        for (name, source) in files {
            std::fs::write(path.join(name), source).unwrap();
        }
        Self(path)
    }

    /// An engine that resolves requires in this directory.
    fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        engine
            .set_module_config(crate::ModuleConfig {
                paths: vec![self.0.clone()],
                ..crate::ModuleConfig::default()
            })
            .unwrap();
        engine
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = std::fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

fn requiring() -> CallOptions {
    CallOptions {
        allow_require: true,
        ..CallOptions::default()
    }
}

/// Calls `run` of `source`, compiled by `engine`, with `values`.
fn invoke(engine: &Engine, source: &str, values: &[Value]) -> Result<crate::Outcome> {
    engine
        .compile(source)
        .unwrap()
        .call("run", values, requiring())
}

/// The codes of the static diagnostics `source` draws.
fn refused(engine: &Engine, source: &str) -> Vec<String> {
    engine
        .compile(source)
        .err()
        .unwrap()
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

/// The file variable `name` in the environment of a file's module that a
/// call returned.
fn variable(value: &Value, name: &str) -> Value {
    let Kind::Namespace(namespace) = &value.0 else {
        panic!("not a namespace: {value}");
    };
    let mut ctx = CallContext::new(CallOptions::default());
    crate::objects::field(&mut ctx, namespace.environment.as_ref().unwrap(), name)
        .unwrap()
        .unwrap()
}

#[test]
fn function_visibility_survives_compilation_and_rejects_invalid_modifiers() {
    let script = file(
        &Engine::new(),
        "private def hidden -> int;7;end\nexport def visible -> int;hidden;end\ndef ordinary -> int;9;end",
    );
    let program = &script.inner.code.program;
    assert!(program.functions[program.names["hidden"]].private);
    assert!(!program.functions[program.names["visible"]].private);
    assert!(!program.functions[program.names["ordinary"]].private);
    assert!(program.functions[0].private);
    assert_eq!(
        script
            .call("visible", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    for source in [
        "export x=1",
        "private x=1",
        "export private def x;1;end",
        "private export def x;1;end",
        "export def self.x;1;end",
        "private def self.x;1;end",
        "def x;export def y;1;end;end",
        "if true;private def x;1;end;end",
        "class C;export def x;1;end;end",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().expect(source).kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn file_variables_survive_initialization_and_share_with_functions_and_methods() {
    let files = Files::new(&[
        (
            "bindings.vibe",
            r#"
x=1
private def bump -> int;x+=1;x;end
def replace -> int;x=7;x;end
def read -> int;x;end
module Bridge
  def self.run -> array<int>;[x,bump,x,replace,x];end
  def self.parameter(x: int) -> array<int>;x+=10;[x,read];end
  def self.read -> int;x;end
end
def run -> array<int>;Bridge.run;end
def parameter(x: int) -> array<int>;Bridge.parameter(x);end
def read_bridge -> int;Bridge.read;end
"#,
        ),
        (
            "fresh.vibe",
            "def create -> int;fresh=9;fresh;end\ndef read_fresh -> int;fresh;end",
        ),
    ]);
    let engine = files.engine();
    let result = invoke(
        &engine,
        "def run -> array<int>;require(\"bindings\").run;end",
        &[],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1, 2, 2, 7, 7]));
    let result = invoke(
        &engine,
        "def run -> array<int>;require(\"bindings\").parameter(5);end",
        &[],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([15, 1]));
    let result = invoke(
        &engine,
        "def run -> int;require(\"bindings\").read_bridge;end",
        &[],
    )
    .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    // A function's local is not a file variable.
    assert_eq!(refused(&engine, "m=require(\"fresh\")"), ["V0201"]);
}

#[test]
fn file_blocks_loops_and_skipped_assignments_keep_their_binding_boundaries() {
    let files = Files::new(&[
        (
            "loops.vibe",
            r#"
x=1
[2].each{|n|x+=n;inner=4}
for i in 1..3
  x+=i
end
def via_block -> int;[2].each{|n|x+=n};x;end
module Bridge
  def self.values -> array<int?>;[x,i];end
  def self.change -> int;via_block;x;end
end
def values -> array<int?>;Bridge.values;end
def change -> int;Bridge.change;end
"#,
        ),
        (
            "boundaries.vibe",
            "[2].each{|n|inner=4}\nif false\n  skipped=9\nend\ndef read_skipped -> int?;skipped;end\ndef read_inner -> int;inner;end",
        ),
    ]);
    let engine = files.engine();
    let result = invoke(
        &engine,
        "def run -> array<any>;m=require(\"loops\");[m.values,m.change,m.values];end",
        &[],
    )
    .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[9, 3], 11, [11, 3]])
    );
    // Neither a skipped assignment nor a block's local is a file variable
    // a function can read.
    assert_eq!(
        refused(&engine, "m=require(\"boundaries\")"),
        ["V0201", "V0201"]
    );
}

#[test]
fn file_global_reads_follow_the_receiving_root_until_locally_assigned() {
    let files = Files::new(&[
        (
            "globals.vibe",
            r#"
initial=limit
def read -> array<int>;[initial,limit];end
def change -> int;limit=33;limit;end
module Bridge
  def self.read -> array<int>;read;end
  def self.change -> int;change;end
  def self.append -> array<int>;items.push(2);items;end
end
def read_bridge -> array<int>;Bridge.read;end
def change_bridge -> int;Bridge.change;end
def append_bridge -> array<int>;Bridge.append;end
"#,
        ),
        ("shared.vibe", "def call_shared -> int;shared;end"),
    ]);
    let mut engine = files.engine();
    engine.declare_global("limit", "int").unwrap();
    engine.declare_global("items", "array<int>").unwrap();
    let options = || {
        let mut options = requiring();
        options.globals.insert("limit".into(), Value::int(5));
        options
            .globals
            .insert("items".into(), Value::array(vec![Value::int(1)]));
        options
    };
    let run = |source: &str| {
        engine
            .compile(source)
            .unwrap()
            .call("run", &[], options())
            .unwrap()
    };
    let result = run(
        "def run -> array<any>;m=require(\"globals\");limit=22;before=m.read_bridge;changed=m.change_bridge;[before,changed,limit,m.read_bridge];end",
    );
    assert_eq!(
        json(&result.value),
        serde_json::json!([[5, 22], 33, 22, [5, 33]])
    );
    let result = run(
        "def run -> array<any>;m=require(\"globals\");old=items;[m.append_bridge,items,old];end",
    );
    assert_eq!(
        json(&result.value),
        serde_json::json!([[1, 2], [1, 2], [1]])
    );
    // A file cannot call a function of the script that requires it.
    assert_eq!(
        refused(&engine, "def shared -> int;17;end\nm=require(\"shared\")"),
        ["V0201"]
    );
}

#[test]
fn file_collection_writes_keep_snapshots_pending_addresses_and_recoverable_state() {
    // Compound writes go through a shape field, since an array element read
    // is optional, and the failed insert is past the end.
    let files = Files::new(&[(
        "collections.vibe",
        r#"
data={items:[{n: 1}]}
def change -> array<array<{ n: int }>>
  old=data["items"]
  data["items"].push({n: 2})
  data["items"][0]["n"]+=data["items"].push({n: 3}).fetch(-1)["n"]
  begin
    data["items"].insert(10, {n: 0})
  rescue
    nil
  end
  [old,data["items"]]
end
module Bridge
  def self.change -> array<array<{ n: int }>>;change;end
  def self.read -> array<{ n: int }>;data["items"];end
end
def change_bridge -> array<array<{ n: int }>>;Bridge.change;end
def read_bridge -> array<{ n: int }>;Bridge.read;end
def bridge -> any;Bridge;end
"#,
    )]);
    let engine = files.engine();
    let result = invoke(
        &engine,
        "def run -> array<any>;m=require(\"collections\");[m.change_bridge,m.read_bridge,m.bridge];end",
        &[],
    )
    .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        json(&Value::array(values[..2].to_vec())),
        serde_json::json!([
            [[{"n": 1}], [{"n": 4}, {"n": 2}, {"n": 3}]],
            [{"n": 4}, {"n": 2}, {"n": 3}]
        ])
    );
    let retained = values[2].clone();
    drop(result);
    let result = invoke(&engine, "def run(m: any) -> any;m;end", &[retained]).unwrap();
    assert_eq!(
        json(&variable(&result.value, "data")),
        serde_json::json!({"items": [{"n": 4}, {"n": 2}, {"n": 3}]})
    );
    let result = invoke(
        &engine,
        "def run -> array<{ n: int }>;require(\"collections\").read_bridge;end",
        &[],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([{"n": 1}]));
}

#[test]
fn reassigning_a_file_function_preserves_argument_evaluation_before_type_errors() {
    let files = Files::new(&[
        (
            "reassign.vibe",
            r#"
def original -> int;7;end
saved=original
original=3
def read -> array<int>;[saved,original];end
module Bridge
  def self.read -> array<int>;read;end
end
def read_bridge -> array<int>;Bridge.read;end
"#,
        ),
        (
            "reject.vibe",
            "def original -> int;7;end\noriginal=3\ndef reject -> int;original(effect());end",
        ),
    ]);
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    let result = invoke(
        &engine,
        "def run -> array<int>;require(\"reassign\").read_bridge;end",
        &[],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([7, 3]));
    // Calling the variable is refused before anything runs.
    assert_eq!(refused(&engine, "m=require(\"reject\")"), ["V0310"]);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn file_type_aliases_and_enum_identity_survive_with_each_environment() {
    let files = Files::new(&[
        (
            "aliases.vibe",
            r#"
enum Status
  Ready
end
type Alias=Status
def accept(value: Alias) -> string;value.name;end
module Bridge
  def self.status -> any;Status;end
  def self.accept(value: Status) -> string;accept(value);end
end
def status -> any;Bridge.status;end
def accept_bridge(value: Status) -> string;Bridge.accept(value);end
"#,
        ),
        (
            "constant.vibe",
            "enum Status\n  Ready\nend\nAlias=Status\ndef accept(value: Alias) -> string;value.name;end",
        ),
    ]);
    let engine = files.engine();
    let status = invoke(
        &engine,
        "def run -> any;require(\"aliases\").status;end",
        &[],
    )
    .unwrap()
    .value;
    let result = invoke(
        &engine,
        "def run(other: any) -> array<any>;m=require(\"aliases\");[m.status==other,m.accept_bridge(:ready)];end",
        &[status],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([true, "Ready"]));
    // A constant holding an enum is not a type; `type` declares an alias.
    assert_eq!(refused(&engine, "m=require(\"constant\")"), ["V0116"]);
}

#[test]
fn file_rescue_declarations_survive_while_exception_bindings_remain_temporary() {
    let files = Files::new(&[
        (
            "rescues.vibe",
            r#"
problem: any=7
seen: string?=nil
begin
  raise("failure")
rescue=>problem
  seen=problem.as(error).message
ensure
  ensured=13
end
module Bridge
  def self.values -> array<any>;[problem,seen,ensured];end
end
def values -> array<any>;Bridge.values;end
"#,
        ),
        (
            "clauses.vibe",
            "begin\n  raise(\"failure\")\n  skipped=9\nrescue\n  rescue_only=11\nend\ndef values -> array<int?>;[skipped,rescue_only];end",
        ),
    ]);
    let engine = files.engine();
    let result = invoke(
        &engine,
        "def run -> array<any>;require(\"rescues\").values;end",
        &[],
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([7, "failure", 13]));
    // Assignments in the begin body after a raise, or in a rescue clause,
    // are not file variables a function can read.
    assert_eq!(
        refused(&engine, "m=require(\"clauses\")"),
        ["V0201", "V0201"]
    );
}

#[test]
fn file_bindings_do_not_shadow_namespace_constants_or_parameters() {
    let files = Files::new(&[(
        "shadow.vibe",
        r#"
DATA=[1]
module Bridge
  DATA=[2,3]
  def self.read -> array<any>;[DATA,DATA.length];end
  def self.parameter(DATA: array<int>) -> array<any>;[DATA,DATA.length];end
end
def read -> array<any>;Bridge.read;end
def parameter(values: array<int>) -> array<any>;Bridge.parameter(values);end
"#,
    )]);
    let result = invoke(
        &files.engine(),
        "def run -> array<any>;m=require(\"shadow\");[m.read,m.parameter([4,5,6])];end",
        &[],
    )
    .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[[2, 3], 2], [[4, 5, 6], 3]])
    );
}

#[test]
fn private_file_state_obeys_receiving_limits_and_cancellation_without_leaking() {
    let files = Files::new(&[(
        "private.vibe",
        r#"
items=[1]
def change -> int;items.push(2);items.push(3);items.length;end
module Bridge
  def self.change -> int;change;end
  def self.stop;cancel();items.push(4);end
  def self.read -> int;items.length;end
end
def change_bridge -> int;Bridge.change;end
def stop_bridge;Bridge.stop;end
def read_bridge -> int;Bridge.read;end
"#,
    )]);
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut engine = files.engine();
    engine.register("cancel", move |_, _| {
        token.cancel();
        Ok(Value::nil())
    });
    let caller = engine
        .compile(
            "def run -> int;require(\"private\").change_bridge;end\ndef stop -> int\n begin\n  require(\"private\").stop_bridge\n  0\n rescue\n  7\n end\nend\ndef read -> int;require(\"private\").read_bridge;end",
        )
        .unwrap();
    // The first call loads the file into the engine's module cache, which
    // later calls reuse.
    caller.call("run", &[], requiring()).unwrap();
    let baseline = caller.call("run", &[], requiring()).unwrap();
    assert_eq!(baseline.value.as_int(), Some(3));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    for (limits, expected) in [
        (
            Limits {
                steps: Some(baseline.stats.steps - 1),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes - 1),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        let error = caller
            .call(
                "run",
                &[],
                CallOptions {
                    limits,
                    ..requiring()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
    let exact = caller
        .call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    steps: Some(baseline.stats.steps),
                    memory_bytes: Some(baseline.stats.peak_memory_bytes),
                    ..Limits::default()
                },
                ..requiring()
            },
        )
        .unwrap();
    assert_eq!(exact.value.as_int(), Some(3));
    assert_eq!(exact.stats.retained_memory_bytes, 0);
    let error = caller
        .call(
            "stop",
            &[],
            CallOptions {
                cancellation,
                ..requiring()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(
        caller
            .call("read", &[], requiring())
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
}
