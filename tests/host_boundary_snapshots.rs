mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Capability, Engine, HostMethod, Signature, SignatureParam, Value};

/// The ADR-004 state the gradual checker's test reads.
const STATE: &str = "
    class Node
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
";

/// A module value has no static type, so the class state that crosses the
/// boundary belongs to a class whose instance carries it.
const TYPED_STATE: &str = "
    class Node
      @next: Node
      @value: int
      def initialize; @value=1; @next=self; end
      def value -> int; @value; end
      def bump -> int; @value+=1; end
      def next_node -> Node; @next; end
    end
    class Counter
      @@value: int=1
      def value -> int; @@value; end
      def bump -> int; @@value+=1; end
    end
    type Pair = { node: Node, counter: Counter }
    type Captured = [Pair, Node]
    node=Node.new
    counter=Counter.new
";

fn capability(methods: &[(&str, HostMethod)]) -> Capability {
    Capability::from_value(
        "cap",
        Value::object(
            methods
                .iter()
                .map(|(name, method)| (name.as_bytes().to_vec(), method.value()))
                .collect(),
        ),
    )
}

fn options(methods: &[(&str, HostMethod)]) -> CallOptions {
    CallOptions {
        capabilities: vec![capability(methods)],
        ..CallOptions::default()
    }
}

/// An engine that declares the `cap` capability `methods` make, and the
/// options that grant it.
fn declared(methods: &[(&str, HostMethod)]) -> (Engine, CallOptions) {
    let mut engine = Engine::new();
    engine.declare_capability(&capability(methods)).unwrap();
    (engine, options(methods))
}

const ARGUMENTS: &str = "
    before=cap.capture({node:node,counter:counter}, alias:node) { node.bump; counter.bump }.as(Captured)
    [before[0][\"node\"].value, before[0][\"counter\"].value,
     before[0][\"node\"]==before[1], before[1]==before[1].next_node,
     node.value, counter.value]
";

#[test]
fn host_arguments_snapshot_state_and_preserve_aliases_across_positional_and_keyword_values() {
    let capture = HostMethod::new_with_block("cap.capture", |call, args, keywords| {
        call.call_block(&[])?;
        call.context()
            .array(&[args[0].clone(), keywords[0].1.clone()])
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = engine
        .compile(&format!("{TYPED_STATE}\n{ARGUMENTS}"))
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, true, true, 2, 2]");
}

#[test]
fn registered_callback_arguments_survive_later_script_mutation() {
    let saved = Arc::new(Mutex::new(None));
    let store = saved.clone();
    let mut engine = Engine::new();
    engine.register_with_keywords("capture", move |ctx, args, keywords| {
        *store.lock().unwrap() = Some(ctx.array(&[args[0].clone(), keywords[0].1.clone()])?);
        Ok(Value::nil())
    });
    engine.register("read", move |_, _| {
        Ok(saved.lock().unwrap().take().unwrap())
    });
    let outcome = engine
        .compile(&format!(
            "{TYPED_STATE}
             capture({{node:node,counter:counter}}, alias:node)
             node.bump; counter.bump
             before=read().as(Captured)
             [before[0][\"node\"].value,before[0][\"counter\"].value,
              before[0][\"node\"]==before[1],before[1]==before[1].next_node]"
        ))
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, true, true]");
}

#[test]
fn retained_host_results_are_isolated_from_script_mutation() {
    let saved = Arc::new(Mutex::new(None));
    let store = saved.clone();
    let retain = HostMethod::new("cap.retain", move |_, args, _| {
        *store.lock().unwrap() = Some(args[0].clone());
        Ok(args[0].clone())
    });
    let read = HostMethod::new("cap.read", move |_, _, _| {
        Ok(saved.lock().unwrap().as_ref().unwrap().clone())
    });
    let (engine, options) = declared(&[("retain", retain), ("read", read)]);
    let outcome = engine
        .compile(&format!(
            "{TYPED_STATE}
             copy=cap.retain({{node:node,counter:counter}}).as(Pair)
             copy[\"node\"].bump; copy[\"counter\"].bump
             before=cap.read().as(Pair)
             [before[\"node\"].value,before[\"counter\"].value,
              node.value,counter.value,copy[\"node\"].value,copy[\"counter\"].value]"
        ))
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 1, 1, 2, 2]");
}

#[test]
fn block_results_are_snapshots_when_the_host_receives_them() {
    let capture = HostMethod::new_with_block("cap.capture", |call, _, _| {
        let before = call.call_block(&[Value::boolean(false)])?;
        call.call_block(&[Value::boolean(true)])?;
        Ok(before)
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = engine
        .compile(&format!(
            "{TYPED_STATE}
             before=cap.capture {{ |mutate|
               if mutate == true; node.bump; counter.bump; end
               {{node:node,counter:counter}}
             }}.as(Pair)
             [before[\"node\"].value,before[\"counter\"].value,node.value,counter.value]"
        ))
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2]");
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn asynchronous_host_arguments_keep_snapshots_across_suspension_and_block_reentry() {
    let capture = HostMethod::new_async("cap.capture", |call, args, keywords| {
        Box::pin(async move {
            tokio::task::yield_now().await;
            call.call_block(vec![]).await?;
            tokio::task::yield_now().await;
            call.context()?
                .array(&[args[0].clone(), keywords[0].1.clone()])
        })
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(
            engine
                .compile(&format!(
                    "{TYPED_STATE}\ndef run -> array<int | bool>\nnode=Node.new\ncounter=Counter.new\n{ARGUMENTS}\nend"
                ))
                .unwrap(),
            "run".into(),
            vec![],
            options,
        )
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, true, true, 2, 2]");
}

#[test]
fn block_arguments_isolate_host_state_and_preserve_aliases_between_slots() {
    let capture = HostMethod::new_with_block("cap.capture", |call, args, _| {
        let changed = call.call_block(&[args[0].clone(), args[0].clone()])?;
        call.context().array(&[args[0].clone(), changed])
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = engine
        .compile(&format!(
            "{TYPED_STATE}
             type Result = [Pair, [int, int, bool]]
             result=cap.capture({{node:node,counter:counter}}) {{ |first,second|
               a=first.as(Pair); b=second.as(Pair)
               a[\"node\"].bump; a[\"counter\"].bump
               [b[\"node\"].value,b[\"counter\"].value,
                a[\"node\"]==b[\"node\"]]
             }}.as(Result)
             [result[0][\"node\"].value,result[0][\"counter\"].value,
              result[1],node.value,counter.value]"
        ))
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, [2, 2, true], 1, 1]");
}

#[test]
fn contracts_retain_isolated_argument_and_result_values() {
    let saved = Arc::new(Mutex::new(Vec::new()));
    let arguments = saved.clone();
    let result = saved.clone();
    let capture = HostMethod::new_with_block("cap.capture", |call, args, _| {
        call.call_block(&[])?;
        Ok(args[0].clone())
    })
    .with_contract(
        move |_, args, _| {
            arguments.lock().unwrap().push(args[0].clone());
            Ok(())
        },
        move |_, value| {
            result.lock().unwrap().push(value.clone());
            Ok(())
        },
    );
    let read = HostMethod::new("cap.read", move |ctx, _, _| {
        ctx.array(&saved.lock().unwrap())
    });
    let (engine, options) = declared(&[("capture", capture), ("read", read)]);
    let outcome = engine
        .compile(&format!(
            "{TYPED_STATE}
             copy=cap.capture(node) {{ node.bump }}.as(Node)
             copy.bump
             type Nodes = array<Node>
             before=cap.read().as(Nodes)
             [before.fetch(0).value,before.fetch(1).value,node.value,copy.value]"
        ))
        .unwrap()
        .run(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2]");
}

#[test]
fn host_boundaries_preserve_the_full_documented_value_depth() {
    let echo = HostMethod::new_with_block("cap.echo", |call, args, keywords| {
        assert_eq!(keywords.len(), 1);
        call.call_block(args)
    });
    let mut input = Value::int(9);
    for _ in 0..10_000 {
        input = Value::array(vec![input]);
    }
    let (mut engine, mut options) = declared(&[("echo", echo)]);
    engine.declare_global("input", "").unwrap();
    options.globals.insert("input".into(), input);
    let outcome = engine
        .compile("cap.echo(input, tag:1) { |value| value }")
        .unwrap()
        .run(options)
        .unwrap();
    let mut cursor = &outcome.value;
    for _ in 0..10_000 {
        cursor = &cursor.as_array().unwrap()[0];
    }
    assert_eq!(cursor.as_int(), Some(9));
}

#[test]
fn snapshots_preserve_declared_types_without_sharing_mutable_state() {
    let echo = HostMethod::new("cap.echo", |_, args, _| Ok(args[0].clone()))
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "node".into(),
                ty: "Node".into(),
                optional: false,
            }],
            result: "Node".into(),
            accepts_block: false,
        })
        .unwrap();
    let state = STATE.replace(
        "def initialize;",
        "@@shared=1; def shared; @@shared; end; def bump_shared; @@shared+=1; end; def initialize;",
    );
    let script = common::gradual_engine()
        .compile(&format!(
            "{state}
             class Holder; property item:Node; end
             def accept(value:Node)->Node; value; end
             def read(value:Node)->int; value.value; end
             def make; cap.echo(Node.new); end
             def run
               node=Node.new
               copy=cap.echo(node)
               copy.bump
               copy.bump_shared
               holder=Holder.new
               holder.item=copy
               [accept(copy).value,node.value,copy.is_a?(Node),copy.is_type?(:Node),holder.item.value,
                copy.shared,node.shared]
             end"
        ))
        .unwrap();
    let output = script
        .call("run", &[], options(&[("echo", echo.clone())]))
        .unwrap();
    assert_eq!(output.value.to_string(), "[2, 1, true, true, 2, 2, 1]");
    let saved = script
        .call("make", &[], options(&[("echo", echo)]))
        .unwrap()
        .value;
    let report = script
        .check_call(
            "read",
            std::slice::from_ref(&saved),
            &CallOptions::default(),
        )
        .unwrap();
    assert!(report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    let output = script
        .call("read", &[saved], CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_int(), Some(1));
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn synchronous_bridge_keeps_host_arguments_isolated() {
    let capture = HostMethod::new_with_block("cap.capture", |call, args, keywords| {
        call.call_block(&[])?;
        call.context()
            .array(&[args[0].clone(), keywords[0].1.clone()])
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(
            engine
                .compile(&format!(
                    "{TYPED_STATE}\ndef run -> array<int | bool>\nnode=Node.new\ncounter=Counter.new\n{ARGUMENTS}\nend"
                ))
                .unwrap(),
            "run".into(),
            vec![],
            options,
        )
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, true, true, 2, 2]");
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn asynchronous_block_results_are_snapshots_at_each_return() {
    let capture = HostMethod::new_async("cap.capture", |call, _, _| {
        Box::pin(async move {
            let before = call.call_block(vec![Value::boolean(false)]).await?;
            tokio::task::yield_now().await;
            call.call_block(vec![Value::boolean(true)]).await?;
            Ok(before)
        })
    });
    let (engine, options) = declared(&[("capture", capture)]);
    let outcome = vibescript::asynchronous::Runner::new(1)
        .unwrap()
        .call(
            engine
                .compile(&format!(
                    "{TYPED_STATE}
                     def run -> array<int>
                       node=Node.new
                       counter=Counter.new
                       before=cap.capture {{ |mutate|
                         if mutate == true; node.bump; counter.bump; end
                         {{node:node,counter:counter}}
                       }}.as(Pair)
                       [before[\"node\"].value,before[\"counter\"].value,node.value,counter.value]
                     end"
                ))
                .unwrap(),
            "run".into(),
            vec![],
            options,
        )
        .await
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[1, 1, 2, 2]");
}
