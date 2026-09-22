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

fn payload() -> Value {
    Engine::new()
        .compile(
            "class Node
               def initialize; @value=1; @next=self; end
               def value; @value; end
               def bump; @value+=1; end
               def next_node; @next; end
             end
             module Counter
               @@value=1
               def self.value; @@value; end
               def self.bump; @@value+=1; end
             end
             node=Node.new
             {node:node, alias:node, counter:Counter}",
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

fn options(method: HostMethod) -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"data".to_vec(), payload()),
                (b"capture".to_vec(), method.value()),
            ]),
        )],
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

const SOURCE: &str = "
    snapshots=cap.capture { cap[:data][:node].bump; cap[:data][:counter].bump }
    old=snapshots[0][:data]
    new=snapshots[1][:data]
    [old[:node].value, old[:counter].value,
     new[:node].value, new[:counter].value,
     old[:node]==old[:alias], old[:node]==old[:node].next_node,
     old[:node]==new[:node]]
";

#[test]
fn receiver_snapshots_isolate_instances_and_module_state_across_block_reentry() {
    let outcome = Engine::new()
        .compile(SOURCE)
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}

#[test]
fn each_receiver_read_has_fresh_state_without_reusing_the_import_cache() {
    let outcome = Engine::new()
        .compile(
            "first=cap.capture { cap[:data][:counter].bump }
             second=cap.capture { cap[:data][:counter].bump }
             [first[0][:data][:counter].value, first[1][:data][:counter].value,
              second[0][:data][:counter].value, second[1][:data][:counter].value,
              cap[:data][:counter].value]",
        )
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 2, 2, 3, 3]");
}

#[test]
fn mutating_a_returned_snapshot_does_not_change_the_receiver_or_another_snapshot() {
    let outcome = Engine::new()
        .compile(
            "snapshots=cap.capture { nil }
             old=snapshots[0][:data]
             old[:node].bump
             old[:counter].bump
             [old[:alias].value, old[:counter].value,
              snapshots[1][:data][:node].value, snapshots[1][:data][:counter].value,
              cap[:data][:node].value, cap[:data][:counter].value]",
        )
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
            Engine::new()
                .compile(&format!("def run\n{SOURCE}\nend"))
                .unwrap(),
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
    let mut engine = Engine::new();
    engine.register("take_snapshot", move |_, _| {
        Ok(read.lock().unwrap().take().unwrap())
    });
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![(b"capture".to_vec(), capture.value())]),
        )],
        ..CallOptions::default()
    };
    (engine, options, calls, saved)
}

const INITIALIZING: &str = "
    module First
      @@value=1
      cap[:data]=First
      cap.capture()
      @@value=2
      def self.value; @@value; end
      def self.later; Later.value; end
    end
    module Later
      @@value=effect()
      def self.value; @@value; end
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
         old=take_snapshot()[:data]
         [old.value, First.value, old.later, Later.value]"
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
        format!("{INITIALIZING}\ndef first; First; end\ndef later; Later; end"),
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
            "m=require(:initializing)
         old=take_snapshot()[:data]
         [old.value, m.first().value, old.later, m.later().value]",
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
    let result = Engine::new()
        .compile("cap.capture { cap[:data][:counter].bump }; nil")
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
    let result = Engine::new()
        .compile("[old[:data].value, old[:data].later]")
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(result.value.to_string(), "[1, nil]");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn vm_snapshots_keep_class_state_shared_inside_each_copy() {
    let result = Engine::new()
        .compile(
            "class Meter
           @@value=1
           def value; @@value; end
           def bump; @@value+=1; end
           def self.value; @@value; end
         end
         cap[:instance]=Meter.new
         cap[:class]=Meter
         snapshots=cap.capture { cap[:instance].bump }
         old=snapshots[0]
         new=snapshots[1]
         first=[old[:instance].value, old[:class].value,
                new[:instance].value, new[:class].value, Meter.value]
         old[:instance].bump
         [first, old[:class].value, new[:class].value, Meter.value]",
        )
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(result.value.to_string(), "[[1, 1, 2, 2, 2], 2, 2, 2]");
}

#[test]
fn transitive_foreign_modules_and_direct_aliases_share_one_snapshot_environment() {
    let result = Engine::new()
        .compile(
            "module Local
           @@peer=cap[:data][:counter]
           @@value=3
           def self.peer; @@peer; end
           def self.value; @@value; end
           def self.bump; @@value+=1; @@peer.bump; end
         end
         cap[:root]=Local
         snapshots=cap.capture { Local.bump }
         old=snapshots[0]
         new=snapshots[1]
         [old[:root].value, old[:root].peer.value,
          new[:root].value, new[:root].peer.value,
          old[:root].peer==old[:data][:counter],
          old[:root].peer==new[:root].peer]",
        )
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(result.value.to_string(), "[3, 1, 4, 2, true, false]");
}

#[test]
fn unbound_module_snapshots_still_read_the_receiving_invocations_ambient_globals() {
    let mut options = options(capture());
    options.globals.insert("count".into(), Value::int(3));
    let result = Engine::new()
        .compile(
            "module Reads
           def self.value; count; end
         end
         cap[:reader]=Reads
         snapshots=cap.capture { count+=1 }
         [snapshots[0][:reader].value, snapshots[1][:reader].value, Reads.value, count]",
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
            Engine::new()
                .compile(&format!("def run\n{SOURCE}\nend"))
                .unwrap(),
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
        .compile("module Deep; @@value=9; def self.value; @@value; end; end; Deep")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    for _ in 0..9_999 {
        value = Value::array(vec![value]);
    }
    let read = HostMethod::new_with_block("cap.read", |call, _, _| Ok(call.receiver()?.unwrap()));
    let result = Engine::new()
        .compile("cap.read()")
        .unwrap()
        .run(CallOptions {
            capabilities: vec![Capability::from_value(
                "cap",
                Value::object(vec![
                    (b"data".to_vec(), value),
                    (b"read".to_vec(), read.value()),
                ]),
            )],
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
    assert_eq!(
        Engine::new()
            .compile("data.value")
            .unwrap()
            .run(options)
            .unwrap()
            .value
            .as_int(),
        Some(9)
    );
    drop(result);
}
