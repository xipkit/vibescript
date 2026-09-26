//! A typed program sees another program's modules and instances as `any`
//! and cannot call them, so these programs snapshot their own state: they
//! push instances and modules into the capability's `slots`, which the
//! capability's callback reads as its receiver, and read the copies back
//! through instances narrowed with `.as`, whose methods run in the copy.

mod common;

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, ModuleConfig, Value,
};

/// The `cap` capability: `slots` for the state a program snapshots, and
/// `method` as `capture`.
fn capability(method: HostMethod) -> Capability {
    Capability::from_value(
        "cap",
        Value::object(vec![
            (b"slots".to_vec(), Value::array(vec![])),
            (b"capture".to_vec(), method.value()),
        ]),
    )
}

/// An engine that declares the `cap` capability with a `capture` method.
fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.declare_capability(&capability(capture())).unwrap();
    engine
}

fn options(method: HostMethod) -> CallOptions {
    CallOptions {
        capabilities: vec![capability(method)],
        ..CallOptions::default()
    }
}

fn capture() -> HostMethod {
    HostMethod::new_with_block("cap.capture", |call, _, _| {
        let before = call.receiver()?.unwrap();
        call.call_block(&[])?;
        let after = call.receiver()?.unwrap();
        call.context().array(&[before, after])
    })
}

/// A node that reads and bumps the `Counter` module in whichever copy of
/// the program's state it belongs to, and helpers to read a snapshot.
const STATE: &str = "
    class Node
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
    def slots(snapshot: any) -> array<any>
      snapshot.as(hash<string, any>).fetch(\"slots\").as(array<any>)
    end
    def node_at(snapshot: any) -> Node
      slots(snapshot).fetch(0).as(Node)
    end
";

const SOURCE: &str = "
    def run -> array<int | bool>
      node=Node.new
      cap.slots.push(node)
      cap.slots.push(node)
      snapshots=cap.capture { node.bump; Counter.bump }.as(array<any>)
      old=slots(snapshots.fetch(0))
      old_node=node_at(snapshots.fetch(0))
      new_node=node_at(snapshots.fetch(1))
      [old_node.value, old_node.counter,
       new_node.value, new_node.counter,
       old_node==old.fetch(1), old_node==old_node.next_node,
       old_node==new_node]
    end
";

#[test]
fn receiver_snapshots_isolate_instances_and_module_state_across_block_reentry() {
    let outcome = engine()
        .compile(&format!("{STATE}{SOURCE}"))
        .unwrap()
        .call("run", &[], options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}

#[test]
fn each_receiver_read_has_fresh_state_without_reusing_the_import_cache() {
    let outcome = engine()
        .compile(&format!(
            "{STATE}
             cap.slots.push(Node.new)
             first=cap.capture {{ Counter.bump }}.as(array<any>)
             second=cap.capture {{ Counter.bump }}.as(array<any>)
             [node_at(first.fetch(0)).counter, node_at(first.fetch(1)).counter,
              node_at(second.fetch(0)).counter, node_at(second.fetch(1)).counter,
              Counter.value]"
        ))
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 2, 2, 3, 3]");
}

#[test]
fn mutating_a_returned_snapshot_does_not_change_the_receiver_or_another_snapshot() {
    let outcome = engine()
        .compile(&format!(
            "{STATE}
             node=Node.new
             cap.slots.push(node)
             cap.slots.push(node)
             snapshots=cap.capture {{ nil }}.as(array<any>)
             old=slots(snapshots.fetch(0))
             old_node=old.fetch(0).as(Node)
             old_node.bump
             old_node.bump_counter
             [old.fetch(1).as(Node).value, old_node.counter,
              node_at(snapshots.fetch(1)).value, node_at(snapshots.fetch(1)).counter,
              node.value, Counter.value]"
        ))
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[2, 2, 1, 1, 1, 1]");
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
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(
            engine().compile(&format!("{STATE}{SOURCE}")).unwrap(),
            "run".into(),
            vec![],
            options(method),
        )
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}

type SnapshotStore = Arc<Mutex<Option<Value>>>;

fn saved_snapshot() -> (Engine, CallOptions, Arc<AtomicUsize>, SnapshotStore) {
    let calls = Arc::new(AtomicUsize::new(0));
    let saved = Arc::new(Mutex::new(None));
    let count = calls.clone();
    let store = saved.clone();
    let capture = HostMethod::new_with_block("cap.capture", move |call, _, _| {
        if count.fetch_add(1, Ordering::SeqCst) != 0 {
            return Err(Error::new(ErrorKind::Runtime, "initializer replayed"));
        }
        *store.lock().unwrap() = call.receiver()?;
        Ok(Value::nil())
    });
    let read = saved.clone();
    let mut engine = engine();
    engine.register("take_snapshot", move |_, _| {
        Ok(read.lock().unwrap().take().unwrap())
    });
    let options = options(capture);
    (engine, options, calls, saved)
}

/// A snapshot taken while `First` initializes, before `Later` has, and a
/// probe that reads both in the copy it belongs to. `Later`'s value is
/// optional because a copy taken before it initializes has none.
const INITIALIZING: &str = "
    class Probe
      def first -> int; First.value; end
      def later -> int?; Later.value; end
      def to_s -> string; [first, later].inspect; end
    end
    module First
      @@value: int=1
      cap.slots.push(Probe.new)
      cap.capture
      @@value=2
      def self.value -> int; @@value; end
    end
    module Later
      @@value: int=effect().as(int)
      def self.value -> int?; @@value; end
    end
";

#[test]
fn unbound_snapshots_capture_partial_initialization_without_replaying_effects() {
    let (mut engine, options, calls, saved) = saved_snapshot();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let result = engine
        .compile(&format!(
            "{INITIALIZING}
         old=take_snapshot().as(hash<string, any>).fetch(\"slots\").as(array<any>).fetch(0).as(Probe)
         [old.first, First.value, old.later, Later.value]"
        ))
        .unwrap()
        .run(options);
    saved.lock().unwrap().take();
    assert_eq!(result.unwrap().value.to_string(), "[1, 2, nil, 7]");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn captured_file_snapshots_do_not_resume_or_replay_source_initializers() {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cache/tmp")
        .join(format!(
            "vm-snapshot-{}-{}",
            common::process_id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("initializing.vibe"),
        format!(
            "{INITIALIZING}
             def first -> int; First.value; end
             def later -> int?; Later.value; end
             def probe(snapshot: any) -> Probe
               snapshot.as(hash<string, any>).fetch(\"slots\").as(array<any>).fetch(0).as(Probe)
             end"
        ),
    )
    .unwrap();
    let (mut engine, mut options, calls, saved) = saved_snapshot();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    engine
        .set_module_config(ModuleConfig {
            paths: vec![dir.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    options.allow_require = true;
    let result = engine
        .compile(
            "m=require(\"initializing\")
         old=m.probe(take_snapshot())
         [old.first, m.first, old.later, m.later]",
        )
        .unwrap()
        .run(options);
    saved.lock().unwrap().take();
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(result.unwrap().value.to_string(), "[1, 2, nil, 7]");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn dropped_vm_snapshots_release_all_invocation_storage() {
    let result = engine()
        .compile(&format!(
            "{STATE}cap.slots.push(Node.new)\ncap.capture {{ Counter.bump }}\nnil"
        ))
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn retained_vm_snapshots_keep_state_when_imported_by_a_later_invocation() {
    let (mut engine, options, calls, saved) = saved_snapshot();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    engine
        .compile(&format!("{INITIALIZING}\nnil"))
        .unwrap()
        .run(options)
        .unwrap();
    let snapshot = saved.lock().unwrap().take().unwrap();
    let mut options = CallOptions::default();
    options.globals.insert("old".into(), snapshot);
    // The later program cannot name the probe's class, so it renders the
    // probe, whose to_s reads the copy.
    let mut reader = Engine::new();
    reader.declare_global("old", "").unwrap();
    let result = reader
        .compile("\"#{old.as(hash<string, any>).fetch(\"slots\").as(array<any>).fetch(0)}\"")
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(result.value.to_string(), "[1, nil]");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn vm_snapshots_keep_class_state_shared_inside_each_copy() {
    let result = engine()
        .compile(
            "class Meter
           @@value: int=1
           def value -> int; @@value; end
           def bump -> int; @@value+=1; end
           def self.value -> int; @@value; end
         end
         def meters(snapshot: any) -> array<Meter>
           snapshot.as(hash<string, any>).fetch(\"slots\").as(array<Meter>)
         end
         meter=Meter.new
         cap.slots.push(meter)
         cap.slots.push(Meter.new)
         snapshots=cap.capture { meter.bump }.as(array<any>)
         old=meters(snapshots.fetch(0))
         new=meters(snapshots.fetch(1))
         first=[old.fetch(0).value, old.fetch(1).value,
                new.fetch(0).value, new.fetch(1).value, Meter.value]
         old.fetch(0).bump
         [first, old.fetch(1).value, new.fetch(1).value, Meter.value]",
        )
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(result.value.to_string(), "[[1, 1, 2, 2, 2], 2, 2, 2]");
}

#[test]
fn transitive_foreign_modules_and_direct_aliases_share_one_snapshot_environment() {
    let result = engine()
        .compile(&format!(
            "{STATE}
         module Local
           @@peer: any=Counter
           @@value: int=3
           def self.peer -> any; @@peer; end
           def self.value -> int; @@value; end
           def self.bump -> int; @@value+=1; Counter.bump; end
         end
         class Probe
           def value -> int; Local.value; end
           def peer -> any; Local.peer; end
         end
         cap.slots.push(Node.new)
         cap.slots.push(Probe.new)
         cap.slots.push(Counter)
         snapshots=cap.capture {{ Local.bump }}.as(array<any>)
         old=slots(snapshots.fetch(0))
         new=slots(snapshots.fetch(1))
         old_probe=old.fetch(1).as(Probe)
         new_probe=new.fetch(1).as(Probe)
         [old_probe.value, node_at(snapshots.fetch(0)).counter,
          new_probe.value, node_at(snapshots.fetch(1)).counter,
          old_probe.peer==old.fetch(2),
          old_probe.peer==new_probe.peer]"
        ))
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(result.value.to_string(), "[3, 1, 4, 2, true, false]");
}

#[test]
fn unbound_module_snapshots_still_read_the_receiving_invocations_ambient_globals() {
    let mut options = options(capture());
    options.globals.insert("count".into(), Value::int(3));
    let mut engine = engine();
    engine.declare_global("count", "int").unwrap();
    let result = engine
        .compile(
            "module Reads
           def self.value -> int; count; end
         end
         class Reader
           def value -> int; Reads.value; end
         end
         def reader(snapshot: any) -> Reader
           snapshot.as(hash<string, any>).fetch(\"slots\").as(array<any>).fetch(0).as(Reader)
         end
         cap.slots.push(Reader.new)
         snapshots=cap.capture { count+=1 }.as(array<any>)
         [reader(snapshots.fetch(0)).value, reader(snapshots.fetch(1)).value, Reads.value, count]",
        )
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(result.value.to_string(), "[4, 4, 4, 4]");
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn synchronous_callbacks_on_the_async_runner_snapshot_vm_state() {
    let result = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(
            engine().compile(&format!("{STATE}{SOURCE}")).unwrap(),
            "run".into(),
            vec![],
            options(capture()),
        )
        .await
        .unwrap();
    assert_eq!(result.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}

#[test]
fn vm_state_in_deep_receivers_uses_the_default_stack_through_copy_and_drop() {
    let mut value = Engine::new()
        .compile(
            "class Deep; @@value: int=9; def value -> int; @@value; end; def to_s -> string; value.to_s; end; end; Deep.new",
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    for _ in 0..9_999 {
        value = Value::array(vec![value]);
    }
    let read = HostMethod::new_with_block("cap.read", |call, _, _| Ok(call.receiver()?.unwrap()));
    let capability = Capability::from_value(
        "cap",
        Value::object(vec![
            (b"data".to_vec(), value),
            (b"read".to_vec(), read.value()),
        ]),
    );
    let mut engine = Engine::new();
    engine.declare_capability(&capability).unwrap();
    let result = engine
        .compile("cap.read")
        .unwrap()
        .run(CallOptions {
            capabilities: vec![capability],
            ..CallOptions::default()
        })
        .unwrap();
    let mut value = &result
        .value
        .as_hash()
        .unwrap()
        .iter()
        .find(|(key, _)| key.as_bytes() == Some(b"data".as_slice()))
        .unwrap()
        .1;
    for _ in 0..9_999 {
        value = &value.as_array().unwrap()[0];
    }
    let mut options = CallOptions::default();
    options.globals.insert("data".into(), value.clone());
    // The reader cannot name the instance's class, so it renders it.
    let mut reader = Engine::new();
    reader.declare_global("data", "").unwrap();
    assert_eq!(
        reader
            .compile("\"#{data}\"")
            .unwrap()
            .run(options)
            .unwrap()
            .value
            .as_bytes(),
        Some(b"9".as_slice())
    );
    drop(result);
}
