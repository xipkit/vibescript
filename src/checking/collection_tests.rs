use super::{
    arguments,
    calls::{self, Analysis, World},
    facts::{Atom, Fact, Facts, Node},
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, budget::Buffer, bytecode};

#[test]
fn array_concatenation_retains_order_and_independent_value_snapshots() {
    for (source, expected) in [
        ("[]+[]", "[]"),
        ("[1]+[2,3]", "[1, 2, 3]"),
        (
            "a=[[1]];b=[[2]];c=a+b;c[0].push(3);[a,b,c]",
            "[[[1]], [[2]], [[1, 3], [2]]]",
        ),
        ("a=[1];b=a+a.push(2);[a,b]", "[[1, 2], [1, 1, 2]]"),
        ("a=[1];a+=a.push(2);a", "[1, 1, 2]"),
    ] {
        super::scope_tests::top(source, expected, false);
    }
    for expression in ["[1]+false", "false+[1]", "[1]+'x'", "'x'+[1]"] {
        super::scope_tests::top(&format!("begin;{expression};rescue;7;end"), "7", true);
    }
}

#[test]
fn array_concatenation_preserves_general_boundaries_and_recursive_growth() {
    use crate::{Engine, Value};
    for (source, rejected) in [
        (
            "def run(a:array<int>,b:array<int>)->array<int>;a+b;end",
            false,
        ),
        (
            "def run(a:array<int>,b:array<bool>)->array<int>;a+b;end",
            true,
        ),
        (
            "def run(a:array<int>,b:array<int>|bool)->array<int>;a+b;end",
            true,
        ),
        ("def run(a:array<int>,b:any)->array<int>;a+b;end", false),
    ] {
        let report = Engine::new()
            .compile(source)
            .unwrap()
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(
            !report.diagnostics.is_empty(),
            rejected,
            "{source}: {report:?}"
        );
    }
    let script = Engine::new()
        .compile("def run(n:int,a=[])->array<int>;if n>0;run(n-1,a+[n]);else;a;end;end")
        .unwrap();
    let args = [Value::int(3)];
    let report = script
        .check_call("run", &args, &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        script
            .call("run", &args, CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "[3, 2, 1]"
    );
}

pub(super) fn analyze(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> Result<Analysis> {
    let program = bytecode::compile(source, Vec::new(), &())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |_, _| Ok(None))?;
        contracts.push(ctx, fact)?;
    }
    let function = program.names["run"];
    let inputs = arguments::general_inputs(
        ctx,
        facts,
        &program.functions[function].params,
        &contracts.data,
    )?;
    calls::analyze(
        ctx,
        facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program: &program,
            contracts: &contracts.data,
            hosts: &[],
            globals: &[],
        },
        function,
        &inputs.data,
    )
}

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
    assert_eq!(
        !result.issues.data.is_empty(),
        rejected,
        "{source}: {result:?}"
    );
    drop((facts, result));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn scalar_literal_facts_keep_values_without_changing_type_boundaries() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let int = facts.integer(&mut ctx, 7).unwrap();
    let text = facts.string(&mut ctx, b"raw\xff").unwrap();
    assert_eq!(int, facts.integer(&mut ctx, 7).unwrap());
    assert_eq!(text, facts.string(&mut ctx, b"raw\xff").unwrap());
    assert_ne!(int, facts.integer(&mut ctx, 8).unwrap());
    assert_ne!(text, facts.string(&mut ctx, b"raw").unwrap());
    for (value, ty) in [(int, Atom::Int), (text, Atom::String)] {
        assert_eq!(
            facts.relation(&mut ctx, value, ty.fact()).unwrap(),
            Relation::Accepted
        );
        assert_eq!(
            facts.relation(&mut ctx, ty.fact(), value).unwrap(),
            Relation::Gradual
        );
        assert_eq!(facts.normalized(&mut ctx, value, ty.fact()).unwrap(), value);
        assert_eq!(
            facts.union(&mut ctx, &[value, ty.fact()]).unwrap(),
            ty.fact()
        );
    }
    let symbol = facts.symbol(&mut ctx, b"raw\xff").unwrap();
    assert_eq!(
        facts.relation(&mut ctx, text, symbol).unwrap(),
        Relation::Rejected
    );
    let a = facts.hash(&mut ctx, text, int).unwrap();
    let b = facts.hash(&mut ctx, symbol, int).unwrap();
    assert_eq!(facts.relation(&mut ctx, a, b).unwrap(), Relation::Accepted);
    let result = facts.scalar_unary(&mut ctx, "-", int).unwrap();
    assert!(matches!(facts.node(result.value), Node::Integer(-7)));
    let min = facts.integer(&mut ctx, i64::MIN).unwrap();
    let negated = facts.scalar_unary(&mut ctx, "-", min).unwrap().value;
    assert_eq!(facts.atom(negated), Some(Atom::Int));
    let bounds = facts.integer_bounds(negated).unwrap();
    assert!(bounds.includes(-i128::from(i64::MIN)));
    assert!(!bounds.includes(0));
}

#[test]
fn indexes_preserve_literal_keys_positions_and_missing_values() {
    for (source, rejected) in [
        ("def run -> int; [7, \"bad\"][0]; end", false),
        ("def run -> int; [7, \"bad\"][1]; end", true),
        ("def run -> string; [7, \"yes\"][-1]; end", false),
        ("def run -> int; index = 0; [7, \"bad\"][index]; end", false),
        (
            "def id(x); x; end; def run -> int; [7, \"bad\"][id(0)]; end",
            false,
        ),
        ("def run -> int?; [7][99]; end", false),
        ("def run -> int; [7][99]; end", true),
        ("def run -> int?; [7][-99]; end", false),
        ("def run -> int?; [7][0.5]; end", false),
        ("def run; [7][true]; end", true),
        ("def run; [7][[0]]; end", true),
        ("def run; [7][\"0\"]; end", true),
        (
            "def run -> int; key = \"x\"; {x:7, y:\"bad\"}[key]; end",
            false,
        ),
        ("def run -> string; {x:7, y:\"yes\"}[:y]; end", false),
        (
            "def id(x); x; end; def run -> int; {x:7, y:\"bad\"}[id(\"x\")]; end",
            false,
        ),
        ("def run -> int; {x:\"bad\", x:7}[:x]; end", false),
        ("def run -> int; {x:7}[:missing]; end", true),
        ("def run -> int?; {x:7}[:missing]; end", false),
        ("def run; {x:7}[1]; end", true),
        ("def run -> string; \"é水\"[-1]; end", false),
        ("def run -> string; \"a\\xff\"[1]; end", false),
        ("def run -> string?; \"é\"[9]; end", false),
        ("def run; \"abc\"[\"a\"]; end", true),
        ("def run; 7[0]; end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn structural_inputs_preserve_field_facts_and_unknown_alternatives() {
    for (source, rejected) in [
        (
            "def run(x: {name: string}) -> string; x[\"name\"]; end",
            false,
        ),
        ("def run(x: {name: string}) -> int; x[\"name\"]; end", true),
        (
            "def run(x: {name?: string}) -> string?; x[:name]; end",
            false,
        ),
        ("def run(x: {name?: string}) -> string; x[:name]; end", true),
        (
            "def run(x: {name: string, ...}) -> int; x[:other]; end",
            false,
        ),
        ("def run(x: array<int>, i: int) -> int?; x[i]; end", false),
        ("def run(x: array<int>, i: int) -> string?; x[i]; end", true),
        (
            "def run(x: hash<string,int>, k: string) -> int?; x[k]; end",
            false,
        ),
        (
            "def run(x: hash<string,int>, k: string) -> string?; x[k]; end",
            true,
        ),
        ("def run(x, i) -> int; x[i]; end", false),
        ("def run(x: int | any) -> int; x[0]; end", true),
        ("def run(x: array<int> | nil) -> int?; x[0]; end", true),
        (
            "def run(flag: bool) -> int; a = flag ? [7] : [8]; a[0]; end",
            false,
        ),
        (
            "def run(flag: bool) -> int; a = flag ? [7] : [\"bad\"]; a[0]; end",
            true,
        ),
        (
            "def run(flag: bool) -> int; key = flag ? :x : :y; {x:7,y:8}[key]; end",
            false,
        ),
        (
            "def run(flag: bool) -> int; key = flag ? :x : :y; {x:7,y:\"bad\"}[key]; end",
            true,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn slices_and_pure_members_keep_value_snapshots_and_container_contents() {
    for (source, rejected) in [
        ("def run -> array<int>; [7, 8, \"bad\"][0,2]; end", false),
        ("def run -> array<int>; [7, \"bad\"][0,2]; end", true),
        ("def run -> array<string>; [7, \"yes\"][-1,1]; end", false),
        ("def run -> array<int>; [7][1,8]; end", false),
        ("def run -> array<int>?; [7][2,8]; end", false),
        ("def run -> array<int>?; [7][0,-1]; end", false),
        ("def run -> array<int>?; [7,8][0..1]; end", false),
        ("def run; [7][0,true]; end", true),
        ("def run; {x:7}[0,1]; end", true),
        ("def run -> int; [7,\"bad\"].first; end", false),
        ("def run -> string; [7,\"yes\"].last; end", false),
        ("def run -> int?; [].first; end", false),
        ("def run -> array<int>; [7,\"bad\"].first(1); end", false),
        ("def run -> array<string>; [7,\"yes\"].last(1); end", false),
        ("def run -> array<int>; [7,\"bad\"].take(1); end", false),
        ("def run -> array<string>; [7,\"yes\"].drop(1); end", false),
        ("def run; [7].first(-1); end", true),
        ("def run; [7].first(\"bad\"); end", true),
        ("def run -> string; [7,\"yes\"].reverse.first; end", false),
        (
            "def run -> int; a = [7,\"bad\"]; b = a.reverse; a.first; end",
            false,
        ),
        ("def run -> int; [7,\"bad\"].at(0); end", false),
        ("def run -> int; [7,\"bad\"].at(*[0]); end", false),
        ("def run; [7].at(0..1); end", true),
        ("def run -> array<int>; [7,\"bad\"].slice(0,1); end", false),
        ("def run -> string?; \"abc\".slice(\"b\"); end", false),
        (
            "def run(flag: bool) -> string?; \"abc\".slice(flag ? \"b\" : 0); end",
            false,
        ),
        ("def run -> int?; \"abc\".getbyte(0); end", false),
        ("def run -> int; [7].size; end", false),
        ("def run -> int; \"é\".bytesize; end", false),
        (
            "def run -> int; if [].empty?; 7; else; \"bad\"; end; end",
            false,
        ),
        (
            "def run -> int; if [1].empty?; \"bad\"; else; 7; end; end",
            false,
        ),
        ("def run -> array<string>; {x:7,y:true}.keys; end", false),
        ("def run -> array<int>; {x:7,y:8}.values; end", false),
        ("def run -> array<int>; {x:7,y:\"bad\"}.values; end", true),
        ("def run -> int; {z:\"bad\",a:7}.values.first; end", true),
        ("def run -> int; [7].dup.first; end", false),
        ("def run -> int; [7].itself.first; end", false),
        ("def run; [7].length(1); end", true),
        ("def run; 7.length; end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn hash_member_reads_keep_field_and_builtin_precedence_separate() {
    for (source, rejected) in [
        ("def run -> int; {x:7}.x; end", false),
        ("def run -> string; {first:\"yes\"}.first; end", false),
        ("def run -> int; {size:\"not size\"}.size; end", false),
        ("def run -> int; {x:7}.size(); end", false),
        ("def run; {x:7}.x(); end", true),
        ("def run; {x:7}.missing; end", true),
        ("def run; {x:7}.missing(JSON.parse(\"1\")); end", true),
        ("def run; {x:7}::x; end", true),
        ("def run(x: int); x::nil?; end", true),
        ("def run(x: int); x::nil?(); end", true),
        ("def run -> string; {x:{name:\"yes\"}}.x.name; end", false),
        ("def run(x: {name: string}) -> string; x.name; end", false),
        ("def run(x: {name: string}) -> int; x.name; end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn unmodeled_collection_operations_remain_explicitly_incomplete() {
    for source in [
        "def run; [7].map! { _1 }; end",
        "def run; [7].first(n: 1); end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!result.incomplete.data.is_empty(), "{source}: {result:?}");
    }
}

#[test]
fn structural_hash_capture_reads_are_modeled() {
    for source in [
        "def run(x: hash); x[0]; end",
        "def run(x: {to_s: string, named_captures: hash}); x[:capture]; end",
    ] {
        check(source, false);
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def id(x); x; end; def run -> int; name=id(\"x\"); {x:[7,\"bad\"], y:[8]}[name].reverse[-1]; end";
    let result = analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    assert!(matches!(facts.node(result.returns), Node::Integer(7)));
    Ok(())
}

#[test]
fn collection_analysis_obeys_exact_limits_and_releases_failed_storage() {
    let mut baseline = CallContext::new(CallOptions::default());
    accounting(&mut baseline).unwrap();
    let stats = baseline.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, error) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let result = accounting(&mut ctx);
        assert_eq!(result.as_ref().err().map(|e| e.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(131) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn literal_and_collection_cache_hits_observe_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let int = facts.integer(&mut ctx, 0).unwrap();
        let text = facts.string(&mut ctx, b"key").unwrap();
        let array = facts.tuple(&mut ctx, &[text]).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let error = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        assert_eq!(facts.integer(&mut ctx, 0).unwrap_err().kind, error);
        assert_eq!(facts.string(&mut ctx, b"key").unwrap_err().kind, error);
        assert_eq!(
            facts
                .collection_index(&mut ctx, array, &[int])
                .err()
                .unwrap()
                .kind,
            error
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

pub(super) fn literal_fact(ctx: &mut CallContext, facts: &mut Facts, value: &crate::Value) -> Fact {
    use crate::value::Kind;
    match &value.0 {
        Kind::Nil => Atom::Nil.fact(),
        Kind::Int(n) => facts.integer(ctx, *n).unwrap(),
        Kind::Big(_) => Atom::Int.fact(),
        Kind::Float(_) => Atom::Float.fact(),
        Kind::Regex(_) => Atom::Regex.fact(),
        Kind::Builtin(builtin) => facts.builtin(ctx, *builtin).unwrap(),
        Kind::Offset(_) => {
            let value = facts.nullable(ctx, Atom::Int.fact()).unwrap();
            let values = facts.array(ctx, value).unwrap();
            facts.offset(ctx, values).unwrap()
        }
        Kind::Time(_) | Kind::Zoned(_) => Atom::Time.fact(),
        Kind::Duration(_) => Atom::Duration.fact(),
        Kind::Money(_) => Atom::Money.fact(),
        Kind::Bool(b) => facts.boolean(ctx, *b).unwrap(),
        Kind::Bytes(b) => facts.string(ctx, &b.data).unwrap(),
        Kind::Symbol(b) => facts.symbol(ctx, &b.data).unwrap(),
        Kind::Range(_) => Atom::Range.fact(),
        Kind::Array(a) => {
            let items: Vec<_> = a
                .buffer
                .data
                .iter()
                .map(|value| literal_fact(ctx, facts, value))
                .collect();
            facts.tuple(ctx, &items).unwrap()
        }
        Kind::Hash(h) => {
            let fields: Vec<_> = h
                .buffer
                .data
                .iter()
                .map(|(key, value)| {
                    (
                        key.as_bytes().unwrap(),
                        literal_fact(ctx, facts, value),
                        false,
                    )
                })
                .collect();
            let shape = facts.shape(ctx, &fields, false).unwrap();
            if h.tag.protected() {
                facts.protected(ctx, shape, h.tag).unwrap()
            } else {
                shape
            }
        }
        _ => panic!("unexpected literal value"),
    }
}

pub(super) fn literal_values() -> Vec<crate::Value> {
    use crate::Value;
    vec![
        Value::nil(),
        Value::boolean(false),
        Value::int(7),
        Value::float(1.5),
        Value::bytes(b""),
        Value::bytes("é水"),
        Value::bytes(b"a\xff"),
        Value::symbol(b"symbol"),
        Value::array(vec![]),
        Value::array(vec![Value::int(7), Value::bytes(b"yes")]),
        Value::hash(vec![]),
        Value::hash(vec![
            (b"x".to_vec(), Value::int(7)),
            (b"size".to_vec(), Value::bytes(b"field")),
        ]),
    ]
}

#[test]
fn inferred_index_results_contain_runtime_values_for_literal_inputs() {
    use crate::Value;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut selectors = vec![
        Value::int(i64::MIN),
        Value::int(-9),
        Value::int(-1),
        Value::int(0),
        Value::int(1),
        Value::int(2),
        Value::int(9),
        Value::int(i64::MAX),
        Value::float(0.5),
        Value::float(f64::NAN),
        Value::bytes(b"x"),
        Value::symbol(b"x"),
        Value::boolean(true),
        Value::array(vec![]),
    ];
    selectors.push(Value(crate::value::Kind::Range(
        crate::range::Range::new(&mut ctx, Some(-1), None, false).unwrap(),
    )));
    let mut cases = 0;
    for receiver in literal_values() {
        let root = literal_fact(&mut ctx, &mut facts, &receiver);
        for selector in &selectors {
            for length in [
                None,
                Some(Value::int(-1)),
                Some(Value::int(0)),
                Some(Value::int(1)),
                Some(Value::int(i64::MAX)),
                Some(Value::bytes(b"bad")),
            ] {
                let mut args = vec![selector.clone()];
                args.extend(length);
                let arguments: Vec<_> = args
                    .iter()
                    .map(|value| literal_fact(&mut ctx, &mut facts, value))
                    .collect();
                let inferred = facts.collection_index(&mut ctx, root, &arguments).unwrap();
                assert!(!inferred.unsupported, "{receiver:?}[{args:?}]");
                let actual = if args.len() == 1 {
                    crate::ops::index(&mut ctx, &receiver, selector)
                } else {
                    crate::sequence::slice(&mut ctx, &receiver, &args, false, None)
                };
                if let Ok(actual) = actual {
                    assert!(
                        !inferred.rejected,
                        "rejected {receiver:?}[{args:?}] = {actual:?}"
                    );
                    let actual = literal_fact(&mut ctx, &mut facts, &actual);
                    assert_ne!(
                        facts.relation(&mut ctx, actual, inferred.value).unwrap(),
                        Relation::Rejected,
                        "{receiver:?}[{args:?}]: inferred={:?}, actual={:?}",
                        facts.node(inferred.value),
                        facts.node(actual)
                    );
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 1080);
}

#[test]
fn inferred_pure_members_contain_runtime_values_for_literal_inputs() {
    use crate::{Value, bytecode::CallSite};
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let names = [
        "length", "size", "bytesize", "empty?", "keys", "values", "reverse", "itself", "dup",
        "nil?", "at", "getbyte", "take", "drop", "first", "last", "x",
    ];
    let arguments = [
        vec![],
        vec![Value::int(-1)],
        vec![Value::int(0)],
        vec![Value::int(1)],
        vec![Value::float(0.5)],
        vec![Value::bytes(b"x")],
        vec![Value::int(0), Value::int(1)],
    ];
    let mut cases = 0;
    for receiver in literal_values() {
        let root = literal_fact(&mut ctx, &mut facts, &receiver);
        for name in names {
            let method = bytecode::Method::parse(name);
            for args in &arguments {
                for scope in [false, true] {
                    let site = CallSite {
                        name: 0,
                        method,
                        auto: args.is_empty(),
                        parenthesized: !args.is_empty(),
                        scope,
                    };
                    let args_facts: Vec<_> = args
                        .iter()
                        .map(|value| literal_fact(&mut ctx, &mut facts, value))
                        .collect();
                    let inferred = facts
                        .collection_member(&mut ctx, root, site, name, &args_facts)
                        .unwrap();
                    if inferred.unsupported {
                        continue;
                    }
                    if let Ok((_, actual)) =
                        crate::members::call(&mut ctx, site, name, receiver.clone(), args)
                    {
                        assert!(
                            !inferred.rejected,
                            "rejected {receiver:?}.{name}({args:?}), scope={scope}: {actual:?}"
                        );
                        let actual = literal_fact(&mut ctx, &mut facts, &actual);
                        assert_ne!(
                            facts.relation(&mut ctx, actual, inferred.value).unwrap(),
                            Relation::Rejected,
                            "{receiver:?}.{name}({args:?}), scope={scope}: inferred={:?}, actual={:?}",
                            facts.node(inferred.value),
                            facts.node(actual)
                        );
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 2786);
}

#[test]
fn collection_reference_differences_have_runtime_witnesses() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-collections.json")).unwrap();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 92);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if case["go_rejected"] == case["rust_rejected"] {
            continue;
        }
        differences += 1;
        assert!(!case["difference"].as_str().unwrap().is_empty());
        let args: Vec<_> = case["runtime"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| {
                if value.is_null() {
                    crate::Value::nil()
                } else {
                    crate::Value::int(value.as_i64().unwrap())
                }
            })
            .collect();
        let actual = crate::Engine::new().compile(source).unwrap().call(
            "run",
            &args,
            CallOptions::default(),
        );
        if let Some(value) = case["runtime"]["value"].as_i64() {
            assert_eq!(actual.unwrap().value.as_int(), Some(value), "{source}");
        } else {
            let kind = match case["runtime"]["error"].as_str().unwrap() {
                "type" => ErrorKind::Type,
                "argument" => ErrorKind::Argument,
                "name" => ErrorKind::Name,
                _ => panic!("unknown runtime expectation"),
            };
            assert_eq!(actual.unwrap_err().kind, kind, "{source}");
        }
    }
    assert_eq!(differences, 26);
}

#[test]
fn structural_hash_inputs_can_have_match_data_indexing() {
    let source = "def make; \"a\".match(/(a)/); end; def run(x: hash) -> string; x[0]; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    assert_ne!(result.throws, 0);
    let script = crate::Engine::new().compile(source).unwrap();
    let matched = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let actual = script
        .call("run", &[matched], CallOptions::default())
        .unwrap();
    assert_eq!(actual.value.as_bytes(), Some(b"a".as_slice()));
}
