use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> String {
    String::from_utf8(
        stringify_json(value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes()
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn run(source: &str) -> String {
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    json(&result.value)
}

fn chain(height: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..height {
        value = Value::array(vec![value]);
    }
    value
}

fn hash_chain(height: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..height {
        value = Value::hash(vec![(b"k".to_vec(), value)]);
    }
    value
}

#[test]
fn nested_equality_policies_are_preserved() {
    assert_eq!(
        run(
            "n=0.0/0.0;a=[[n]];[[[1]]==[[1.0]],[[1]].eql?([[1.0]]),{a:[1]}=={a:[1.0]},\
             {a:[1]}.eql?({a:[1.0]}),[[n]]==[[n]],a==a,{a:{b:[1,:x]}}=={a:{b:[1,\"x\"]}},\
             [[1,[2]]]==[[1,[2,3]]],[[1],[2]] != [[1],[3]],{a:{b:1}}=={b:{a:1}},[{}]==[{}],[[]]==[[]]]"
        ),
        "[true,false,true,false,false,false,false,false,true,false,true,true]"
    );
}

#[test]
fn nested_ordering_is_lexicographic_with_early_exits() {
    assert_eq!(
        run(
            "[[[1,2],[9]]<=>[[1,3],[0]],[[1]]<=>[[1,2]],[[1]]<=>[[\"a\"]],[[1,2]]<=>[[1,2],[0]],\
             [[1,2],[9]]<=>[[1,2],[9]],[[[1,2],[9]],[[1,2],[0]],[[1,1],[5]]].sort,\
             [[[1,2],[9]],[[1,2],[0]],[[1,1],[5]]].min,[[[1,2],[9]],[[1,2],[0]],[[1,1],[5]]].max]"
        ),
        "[-1,-1,null,-1,0,[[[1,1],[5]],[[1,2],[0]],[[1,2],[9]]],[[1,1],[5]],[[1,2],[9]]]"
    );
}

#[test]
fn join_separators_and_flatten_depth_forms_are_preserved() {
    assert_eq!(
        run(
            "[[[1,[2]],3].join(\"-\"),[[],1].join(\",\"),[[1],[]].join(\",\"),[[nil,true],1.5].join(\"/\"),\
             [1,[2,[3,[4]]]].flatten,[1,[2,[3,[4]]]].flatten(1),[1,[2,[3,[4]]]].flatten(0),\
             [1,[2,[3,[4]]]].flatten(nil),{a:[1,[2]]}.flatten,{a:[1,[2]]}.flatten(2),\
             {a:[1,[2]]}.flatten(-1),{a:[1,[2]]}.flatten(0)]"
        ),
        "[\"1-2-3\",\",1\",\"1,\",\"/true/1.5\",[1,2,3,4],[1,2,[3,[4]]],[1,[2,[3,[4]]]],[1,2,3,4],\
         [\"a\",[1,[2]]],[\"a\",1,[2]],[\"a\",1,2],[[\"a\",[1,[2]]]]]"
    );
}

#[test]
fn deep_host_values_walk_every_importable_level() {
    let script = Engine::new()
        .compile(
            "def probe(x)\n1\nend\ndef same(a,b)\na==b\nend\ndef strict(a,b)\na.eql?(b)\nend\n\
             def cmp(a,b)\na<=>b\nend\ndef joined(a)\na.join(\"-\")\nend\ndef flat(a)\na.flatten.length\nend",
        )
        .unwrap();
    for height in [128, 10_000] {
        let a = chain(height, Value::int(1));
        let b = chain(height, Value::int(1));
        let c = chain(height, Value::int(2));
        let ha = hash_chain(height, Value::int(1));
        let hb = hash_chain(height, Value::int(1));
        let call = |name: &str, args: &[Value]| {
            let outcome = script
                .call(name, args, CallOptions::default())
                .unwrap_or_else(|error| panic!("{name} at height {height}: {error}"));
            if name == "joined" {
                assert!(outcome.stats.retained_memory_bytes < 512);
            } else {
                assert_eq!(
                    outcome.stats.retained_memory_bytes, 0,
                    "{name} at height {height}"
                );
            }
            json(&outcome.value)
        };
        assert_eq!(call("same", &[a.clone(), b.clone()]), "true");
        assert_eq!(call("same", &[a.clone(), c.clone()]), "false");
        assert_eq!(call("strict", &[a.clone(), b.clone()]), "true");
        assert_eq!(call("same", &[ha.clone(), hb.clone()]), "true");
        assert_eq!(call("strict", &[ha.clone(), hb.clone()]), "true");
        assert_eq!(call("cmp", &[a.clone(), b.clone()]), "0");
        assert_eq!(call("cmp", &[a.clone(), c.clone()]), "-1");
        assert_eq!(call("cmp", &[c.clone(), a.clone()]), "1");
        assert_eq!(call("joined", std::slice::from_ref(&a)), "\"1\"");
        assert_eq!(call("flat", std::slice::from_ref(&a)), "1");
    }
}

#[test]
fn shared_graph_equality_compares_each_shared_pair_once() {
    // 2^40 paths through 40 distinct pairs of shared arrays.
    let outcome = Engine::new()
        .compile("a=[0];b=[0];40.times {a=[a,a];b=[b,b]};a==b")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                steps: Some(50_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(outcome.value.type_name(), "bool");
    assert!(outcome.value.truthy());
    assert_eq!(outcome.stats.retained_memory_bytes, 0);
}

#[test]
fn cancellation_during_collection_walks_prevents_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "a==b",
        "a.eql?(b)",
        "a<=>b",
        "[a,b].sort",
        "a.join(\",\")",
        "a.flatten",
    ] {
        let error = engine
            .compile(&format!(
                "a=(1..64).to_a.chunk(4);b=(1..64).to_a.chunk(4);cancel();{expression};effect()"
            ))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}
