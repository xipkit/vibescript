mod common;

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Capability, Engine, HostMethod, ModuleConfig, Value};

fn payload() -> Value {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cache/tmp")
        .join(format!(
            "snapshot-{}-{}",
            common::process_id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("state.vibe"),
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
             def payload
               node=Node.new
               {node:node, alias:node, counter:Counter}
             end",
    )
    .unwrap();
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![dir.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let result = engine
        .compile("require(:state).payload()")
        .unwrap()
        .run(CallOptions {
            allow_require: true,
            ..CallOptions::default()
        });
    fs::remove_dir_all(dir).unwrap();
    result.unwrap().value
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
            "first=cap.capture { cap[:data][:node].bump }
             second=cap.capture { cap[:data][:node].bump }
             [first[0][:data][:node].value, first[1][:data][:node].value,
              second[0][:data][:node].value, second[1][:data][:node].value,
              cap[:data][:node].value]",
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

#[test]
fn receiver_reads_snapshot_state_when_requested_after_arguments_run() {
    let outcome = Engine::new()
        .compile(
            "snapshots=cap.capture(cap[:data][:node].bump) { cap[:data][:node].bump }
             [snapshots[0][:data][:node].value, snapshots[1][:data][:node].value]",
        )
        .unwrap()
        .run(options(capture()))
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[2, 3]");
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

#[cfg(feature = "tokio")]
#[tokio::test]
async fn bridged_sync_callbacks_keep_the_same_snapshot_isolation() {
    let outcome = vibescript::asynchronous::Runner::new(1)
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
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2, true, true, false]");
}
