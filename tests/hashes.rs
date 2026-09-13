use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, parse_json, stringify_json};

fn object(size: usize) -> Value {
    Value::hash(
        (0..size)
            .map(|i| (format!("k{i:05}").into_bytes(), Value::int(i as i64)))
            .collect(),
    )
}

#[test]
fn duplicates_and_growth_preserve_order_and_lookup() {
    for size in [0, 1, 15, 16, 17, 24, 25, 128, 1024] {
        let mut entries: Vec<_> = (0..size).map(|i| format!("\"k{i:05}\":{i}")).collect();
        entries.extend((0..size).rev().map(|i| format!("\"k{i:05}\":{}", i + 7)));
        let input = format!("{{{}}}", entries.join(","));
        let parsed = parse_json(input.as_bytes(), CallOptions::default()).unwrap();
        let expected = format!(
            "{{{}}}",
            (0..size)
                .map(|i| format!("\"k{i:05}\":{}", i + 7))
                .collect::<Vec<_>>()
                .join(",")
        );
        let encoded = stringify_json(&parsed.value, CallOptions::default()).unwrap();
        assert_eq!(encoded.value.as_bytes(), Some(expected.as_bytes()));
        let script = Engine::new()
            .compile("def run(h)\n keys=h.keys\n i=0\n total=0\n while i<keys.length\n total+=h[keys[i]]\n i+=1\n end\n [total,h[\"missing\"]]\nend")
            .unwrap();
        let output = script
            .call("run", &[parsed.value], CallOptions::default())
            .unwrap();
        let array = output.value.as_array().unwrap();
        assert_eq!(
            array[0].as_int(),
            Some((0..size as i64).map(|i| i + 7).sum())
        );
        assert_eq!(array[1].type_name(), "nil");
    }
}

#[test]
fn updates_preserve_snapshots_including_self_references() {
    let script = Engine::new().compile(
        "def run(h)\n old=h\n h[:k00000]=9\n h[\"self\"]=h\n h[\"new\"]=7\n old[\"other\"]=8\n [h[:k00000],old[:k00000],h[\"self\"].length,h.length,old.length,h[\"other\"],old[\"new\"]]\nend"
    ).unwrap();
    for size in [15, 16, 24, 25, 512] {
        let input = object(size);
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
        let expected = format!("[9,0,{size},{},{},null,null]", size + 2, size + 1);
        assert_eq!(encoded.value.as_bytes(), Some(expected.as_bytes()));
        assert_eq!(input.as_hash().unwrap().len(), size);
        assert_eq!(input.as_hash().unwrap()[0].1.as_int(), Some(0));
    }
}

#[test]
fn growth_has_bounded_work_and_memory() {
    let script = Engine::new()
        .compile("h={}\ni=0\nwhile i<2000\n h[i.to_s]=i\n i+=1\nend\nh[\"1999\"]")
        .unwrap();
    let output = script
        .run(CallOptions {
            limits: Limits {
                steps: Some(150_000),
                memory_bytes: Some(1 << 20),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(output.value.as_int(), Some(1999));
    assert_eq!(output.stats.retained_memory_bytes, 0);
}

#[test]
fn replacing_deepest_value_updates_depth() {
    let mut deep = Value::nil();
    for _ in 0..100 {
        deep = Value::array(vec![deep]);
    }
    let input = Value::hash(vec![(b"deep".to_vec(), deep)]);
    let source = "def run(h,n)\n h[:deep]=1\n i=0\n while i<n\n h={child:h}\n i+=1\n end\n h\nend";
    let script = Engine::new().compile(source).unwrap();
    script
        .call(
            "run",
            &[input.clone(), Value::int(127)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        script
            .call("run", &[input, Value::int(128)], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
}

#[test]
fn ordered_hash_equality_uses_values_and_ignores_order() {
    let first = object(512);
    let reversed = Value::hash(
        first
            .as_hash()
            .unwrap()
            .iter()
            .rev()
            .map(|(k, v)| (k.as_bytes().unwrap().to_vec(), v.clone()))
            .collect(),
    );
    let script = Engine::new()
        .compile("def run(a,b)\n same=a==b\n b[:k00000]=-1\n [same,a==b]\nend")
        .unwrap();
    let output = script
        .call("run", &[first, reversed], CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(b"[true,false]".as_slice()));
}

#[test]
fn removals_and_reinsertion_preserve_lookup_order_and_snapshots() {
    let source = "def run(h)\nold=h\nkeys=h.keys\nremoved=[]\ni=0\nwhile i<keys.length\nremoved.push(h.delete(keys[i]))\ni+=2\nend\nh.store(:k00000,-1)\n[h.keys,h.values,removed,old]\nend";
    let script = Engine::new().compile(source).unwrap();
    for size in [0, 1, 15, 16, 17, 24, 25, 128, 512] {
        let input = object(size);
        let result = script
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        steps: Some(5_000_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        let values = result.value.as_array().unwrap();
        let keys = values[0].as_array().unwrap();
        let remaining = values[1].as_array().unwrap();
        for (slot, i) in (1..size).step_by(2).enumerate() {
            assert_eq!(keys[slot].as_bytes(), Some(format!("k{i:05}").as_bytes()));
            assert_eq!(remaining[slot].as_int(), Some(i as i64));
        }
        assert_eq!(keys.len(), size / 2 + 1);
        assert_eq!(remaining.len(), keys.len());
        assert_eq!(keys.last().unwrap().as_bytes(), Some(b"k00000".as_slice()));
        assert_eq!(remaining.last().unwrap().as_int(), Some(-1));
        assert_eq!(
            values[2]
                .as_array()
                .unwrap()
                .iter()
                .map(Value::as_int)
                .collect::<Vec<_>>(),
            (0..size)
                .step_by(2)
                .map(|i| Some(i as i64))
                .collect::<Vec<_>>()
        );
        assert_eq!(values[3].as_hash().unwrap().len(), size);
        for (i, (_, value)) in input.as_hash().unwrap().iter().enumerate() {
            assert_eq!(value.as_int(), Some(i as i64));
        }
    }
}

#[test]
fn removing_deepest_values_releases_nesting_depth() {
    let mut deep = Value::nil();
    for _ in 0..100 {
        deep = Value::array(vec![deep]);
    }
    for (input, removal) in [
        (
            Value::hash(vec![(b"deep".to_vec(), deep.clone())]),
            "h.delete(:deep)",
        ),
        (Value::array(vec![deep.clone()]), "h.pop"),
        (Value::array(vec![deep]), "h.shift"),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(h)\n{removal}\nfor i in 1..127\nh=[h]\nend\nh\nend"
            ))
            .unwrap();
        script
            .call("run", &[input], CallOptions::default())
            .unwrap();
    }
}
