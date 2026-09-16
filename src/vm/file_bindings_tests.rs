use super::*;
use crate::{CallOptions, CancellationToken, Engine, Limits, Script, ScriptInner};
use std::sync::atomic::{AtomicUsize, Ordering};

fn file(engine: &Engine, source: &str) -> Script {
    Script {
        inner: Arc::new(ScriptInner {
            code: crate::code::Code::compile_file(source, &engine.hosts).unwrap(),
            loader: engine.loader.clone(),
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

fn invoke(source: &str, values: &[Value]) -> Result<crate::Outcome> {
    Engine::new()
        .compile(source)
        .unwrap()
        .call("run", values, CallOptions::default())
}

#[test]
fn function_visibility_survives_compilation_and_rejects_invalid_modifiers() {
    let script = file(
        &Engine::new(),
        "private def hidden;7;end\nexport def visible;hidden();end\ndef ordinary;9;end",
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
    let script = file(
        &Engine::new(),
        r#"
x=1
private def bump;x+=1;x;end
def replace;x=7;x;end
def read;x;end
def create;fresh=9;fresh;end
def read_fresh;fresh;end
module Bridge
  def self.run;[x,bump(),x,replace(),x];end
  def self.parameter(x);x+=10;[x,read()];end
  def self.read;x;end
  def self.new_local;create();read_fresh();end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke("def run(m);m.run;end", std::slice::from_ref(&value)).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1, 2, 2, 7, 7]));
    let result = invoke(
        "def run(m);m.parameter(5);end",
        std::slice::from_ref(&value),
    )
    .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([15, 1]));
    let result = invoke("def run(m);m.read;end", std::slice::from_ref(&value)).unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let error = invoke("def run(m);m.new_local;end", &[value]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
}

#[test]
fn file_blocks_loops_and_skipped_assignments_keep_their_binding_boundaries() {
    let script = file(
        &Engine::new(),
        r#"
x=1
[2].each{|n|x+=n;inner=4}
if false
  skipped=9
end
for i in 1..3
  x+=i
end
def via_block;[2].each{|n|x+=n};x;end
module Bridge
  def self.values;[x,skipped,i];end
  def self.change;via_block();x;end
  def self.hidden_inner;inner;end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke(
        "def run(m);[m.values,m.change,m.values];end",
        std::slice::from_ref(&value),
    )
    .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[9, null, 3], 11, [11, null, 3]])
    );
    let error = invoke("def run(m);m.hidden_inner;end", &[value]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
}

#[test]
fn file_global_reads_follow_the_receiving_root_until_locally_assigned() {
    let script = file(
        &Engine::new(),
        r#"
initial=Math.PI
def read;[initial,Math.PI];end
def change;Math={PI:33};Math.PI;end
module Bridge
  def self.read;read();end
  def self.change;change();end
  def self.append;Math.data.push(2);Math.data;end
  def self.shared;shared();end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke("def run(m);Math={PI:22};before=m.read;changed=m.change;[before,changed,Math.PI,m.read];end", std::slice::from_ref(&value)).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            [std::f64::consts::PI, 22],
            33,
            22,
            [std::f64::consts::PI, 33]
        ])
    );
    let result = invoke(
        "def run(m);Math={data:[1]};old=Math.data;[m.append,Math.data,old];end",
        std::slice::from_ref(&value),
    )
    .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[1, 2], [1, 2], [1]])
    );
    let result = invoke("def shared;17;end\ndef run(m);m.shared;end", &[value]).unwrap();
    assert_eq!(result.value.as_int(), Some(17));
}

#[test]
fn file_collection_writes_keep_snapshots_pending_addresses_and_recoverable_state() {
    let script = file(
        &Engine::new(),
        r#"
data={items:[1]}
def change
  old=data.items
  data.items.push(2)
  data.items[0]+=data.items.push(3).last
  begin
    data.items.insert()
  rescue
    nil
  end
  [old,data.items]
end
module Bridge
  def self.change;change();end
  def self.read;data.items;end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke(
        "def run(m);[m.change,m.read,m];end",
        std::slice::from_ref(&value),
    )
    .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        json(&Value::array(values[..2].to_vec())),
        serde_json::json!([[[1], [4, 2, 3]], [4, 2, 3]])
    );
    let retained = values[2].clone();
    drop(result);
    let result = invoke("def run(m);m.read;end", &[retained]).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([4, 2, 3]));
    let result = invoke("def run(m);m.read;end", &[value]).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1]));
}

#[test]
fn reassigning_a_file_function_preserves_argument_evaluation_before_type_errors() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    let script = file(
        &engine,
        r#"
def original;7;end
saved=original()
original=3
def read;[saved,original];end
def reject;original(effect());end
module Bridge
  def self.read;read();end
  def self.reject;reject();end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke("def run(m);m.read;end", std::slice::from_ref(&value)).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([7, 3]));
    let error = invoke("def run(m);m.reject;end", &[value]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn file_type_aliases_and_enum_identity_survive_with_each_environment() {
    let script = file(
        &Engine::new(),
        r#"
enum Status
  Ready
end
Alias=Status
def accept(value: Alias);value.name;end
def replace;Alias=7;end
module Bridge
  def self.status;Status;end
  def self.accept(value);accept(value);end
  def self.replace;replace();end
end
Bridge
"#,
    );
    let a = script.run(CallOptions::default()).unwrap().value;
    let b = script.run(CallOptions::default()).unwrap().value;
    let result = invoke(
        "def run(a,b);[a.status==b.status,a.status.equal?(b.status),a.accept(:ready),b.accept(:ready)];end",
        &[a.clone(), b],
    )
    .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([true, false, "Ready", "Ready"])
    );
    let error = invoke("def run(m);m.replace;m.accept(:ready);end", &[a]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
}

#[test]
fn file_rescue_declarations_survive_while_exception_bindings_remain_temporary() {
    let script = file(
        &Engine::new(),
        r#"
error=7
seen=nil
begin
  raise("failure")
  skipped=9
rescue=>error
  seen=error.message
  rescue_only=11
ensure
  ensured=13
end
module Bridge
  def self.values;[error,seen,skipped,rescue_only,ensured];end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke("def run(m);m.values;end", &[value]).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([7, "failure", null, 11, 13])
    );
}

#[test]
fn file_bindings_do_not_shadow_namespace_constants_or_parameters() {
    let script = file(
        &Engine::new(),
        r#"
DATA=[1]
module Bridge
  DATA=[2,3]
  def self.read;[DATA,DATA.size];end
  def self.parameter(DATA);[DATA,DATA.size];end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let result = invoke("def run(m);[m.read,m.parameter([4,5,6])];end", &[value]).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[[2, 3], 2], [[4, 5, 6], 3]])
    );
}

#[test]
fn private_file_state_obeys_receiving_limits_and_cancellation_without_leaking() {
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut engine = Engine::new();
    engine.register("cancel", move |_, _| {
        token.cancel();
        Ok(Value::nil())
    });
    let script = file(
        &engine,
        r#"
items=[1]
def change;items.push(2);items.push(3);items.size;end
module Bridge
  def self.change;change();end
  def self.stop;cancel();items.push(4);end
  def self.read;items.size;end
end
Bridge
"#,
    );
    let value = script.run(CallOptions::default()).unwrap().value;
    let caller = Engine::new()
        .compile("def run(m);m.change;end\ndef stop(m);begin;m.stop;rescue;7;end;end")
        .unwrap();
    let args = std::slice::from_ref(&value);
    let baseline = caller.call("run", args, CallOptions::default()).unwrap();
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
                args,
                CallOptions {
                    limits,
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
    let exact = caller
        .call(
            "run",
            args,
            CallOptions {
                limits: Limits {
                    steps: Some(baseline.stats.steps),
                    memory_bytes: Some(baseline.stats.peak_memory_bytes),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(exact.value.as_int(), Some(3));
    assert_eq!(exact.stats.retained_memory_bytes, 0);
    let error = caller
        .call(
            "stop",
            args,
            CallOptions {
                cancellation,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(
        invoke("def run(m);m.read;end", args)
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
}
