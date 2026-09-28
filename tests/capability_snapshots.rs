mod common;

use vibescript::{CallOptions, Capability, Engine, HostMethod, Script, Value};

/// The receiving program. `payload` builds the capability's data: a node, an
/// alias of it and a module whose state the node's methods reach. No program
/// can name another program's classes, so the data comes from an earlier
/// call of this same script, whose instances it narrows with `.as(Node)`.
const PROGRAM: &str = "class Node
  @next: Node
  @value: int
  def initialize; @value=1; @next=self; end
  def value -> int; @value; end
  def bump -> int; @value+=1; end
  def next_node -> Node; @next; end
  def counter -> int; Counter.value; end
  def bump_counter -> int; Counter.bump; end
end
module Counter
  @@value: int=1
  def self.value -> int; @@value; end
  def self.bump -> int; @@value+=1; end
end
def payload -> { alias: Node, counter: any, node: Node }
  node=Node.new
  {node:node, alias:node, counter:Counter}
end
def data(receiver: any) -> hash<string, any>
  receiver.as(hash<string, any>).fetch(\"data\").as(hash<string, any>)
end
def node(data: hash<string, any>) -> Node
  data.fetch(\"node\").as(Node)
end
def isolate -> array<any>
  snapshots=cap.capture { cap.data[\"node\"].as(Node).bump; cap.data[\"node\"].as(Node).bump_counter }.as(array<any>)
  old=data(snapshots.fetch(0))
  new=data(snapshots.fetch(1))
  [node(old).value, node(old).counter, node(new).value, node(new).counter,
   old[\"node\"]==old[\"alias\"], node(old)==node(old).next_node, old[\"node\"]==new[\"node\"]]
end
def fresh -> array<int>
  first=cap.capture { cap.data[\"node\"].as(Node).bump }.as(array<any>)
  second=cap.capture { cap.data[\"node\"].as(Node).bump }.as(array<any>)
  [node(data(first.fetch(0))).value, node(data(first.fetch(1))).value,
   node(data(second.fetch(0))).value, node(data(second.fetch(1))).value,
   cap.data[\"node\"].as(Node).value]
end
def mutate -> array<int>
  snapshots=cap.capture { nil }.as(array<any>)
  old=data(snapshots.fetch(0))
  node(old).bump
  node(old).bump_counter
  [old.fetch(\"alias\").as(Node).value, node(old).counter,
   node(data(snapshots.fetch(1))).value, node(data(snapshots.fetch(1))).counter,
   cap.data[\"node\"].as(Node).value, cap.data[\"node\"].as(Node).counter]
end
def after_arguments -> array<int>
  snapshots=cap.capture(cap.data[\"node\"].as(Node).bump) { cap.data[\"node\"].as(Node).bump }.as(array<any>)
  [node(data(snapshots.fetch(0))).value, node(data(snapshots.fetch(1))).value]
end
";

fn capability(method: &HostMethod, data: Value) -> Capability {
    Capability::from_value(
        "cap",
        Value::object(vec![
            (b"data".to_vec(), data),
            (b"capture".to_vec(), method.value()),
        ]),
    )
}

fn options(method: &HostMethod, data: Value) -> CallOptions {
    CallOptions {
        capabilities: vec![capability(method, data)],
        ..CallOptions::default()
    }
}

/// Data of the payload's shape whose fields are instances of a class no
/// program here names, so the template declares them as `any`.
fn opaque() -> Value {
    let instance = Engine::new()
        .compile("class Opaque\nend\nOpaque.new")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    Value::object(vec![
        (b"node".to_vec(), instance.clone()),
        (b"alias".to_vec(), instance.clone()),
        (b"counter".to_vec(), instance),
    ])
}

/// Compiles the receiving program with `cap` declared, and returns it with
/// the data its own `payload` builds.
fn compile(method: &HostMethod) -> (Script, Value) {
    let mut engine = Engine::new();
    engine
        .declare_capability(&capability(method, opaque()))
        .unwrap();
    let script = engine.compile(PROGRAM).unwrap();
    let data = script
        .call("payload", &[], options(method, opaque()))
        .unwrap()
        .value;
    (script, data)
}

/// Calls `function` of the receiving program with its data granted.
fn run(function: &str, method: HostMethod) -> String {
    let (script, data) = compile(&method);
    script
        .call(function, &[], options(&method, data))
        .unwrap()
        .value
        .to_string()
}

fn capture() -> HostMethod {
    HostMethod::new_with_block("cap.capture", |call, _, _| {
        let before = call.receiver()?.unwrap();
        call.call_block(&[])?;
        let after = call.receiver()?.unwrap();
        call.context().array(&[before, after])
    })
}

#[test]
fn static_types_refuse_calling_the_capability_data() {
    let mut engine = vibescript::Engine::new();
    engine
        .declare_capability(&capability(&capture(), opaque()))
        .unwrap();
    let source = "snapshots=cap.capture { cap.data[\"node\"].bump }";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0106"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("bump").unwrap()
    );
}

#[test]
fn receiver_snapshots_isolate_instances_and_module_state_across_block_reentry() {
    assert_eq!(run("isolate", capture()), "[1, 1, 2, 2, true, true, false]");
}

#[test]
fn each_receiver_read_has_fresh_state_without_reusing_the_import_cache() {
    assert_eq!(run("fresh", capture()), "[1, 2, 2, 3, 3]");
}

#[test]
fn mutating_a_returned_snapshot_does_not_change_the_receiver_or_another_snapshot() {
    assert_eq!(run("mutate", capture()), "[2, 2, 1, 1, 1, 1]");
}

#[test]
fn receiver_reads_snapshot_state_when_requested_after_arguments_run() {
    assert_eq!(run("after_arguments", capture()), "[2, 3]");
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn async_receiver_snapshots_survive_suspension_and_block_reentry() {
    let method = HostMethod::new_async("cap.capture", |call, _, _| {
        Box::pin(async move {
            let before = call.receiver()?.unwrap();
            tokio::task::yield_now().await;
            call.call_block(vec![]).await?;
            tokio::task::yield_now().await;
            let after = call.receiver()?.unwrap();
            call.context()?.array(&[before, after])
        })
    });
    let (script, data) = compile(&method);
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(script, "isolate".into(), vec![], options(&method, data))
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn bridged_sync_callbacks_keep_the_same_snapshot_isolation() {
    let method = capture();
    let (script, data) = compile(&method);
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(script, "isolate".into(), vec![], options(&method, data))
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}
