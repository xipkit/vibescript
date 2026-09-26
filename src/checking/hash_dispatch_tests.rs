//! Structural hash and shape member dispatch: plain hashes and host objects
//! admitted by the same contract dispatch differently, and analysis must model
//! both without executing stored callbacks.

use super::{collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime};
use crate::{CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn plain() -> Value {
    Value::hash(vec![(b"a".to_vec(), Value::int(1))])
}

fn object() -> Value {
    Value::object(vec![(b"a".to_vec(), Value::int(1))])
}

fn both() -> [Value; 2] {
    [plain(), object()]
}

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn runtime_error(source: &str, args: &[Value]) -> String {
    Engine::new()
        .compile(source)
        .unwrap()
        .call("run", args, CallOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{source}: expected a runtime failure"))
        .to_string()
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

/// The runtime fails for this input, but the checker cannot prove it: the
/// report is clean and complete while retaining the ordinary error path.
fn possible_failure(source: &str, args: &[Value]) -> String {
    let message = runtime_error(source, args);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert!(report.issues.data.is_empty(), "{source}: {report:?}");
    assert_ne!(report.throws, 0, "{source}: {report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    message
}

#[test]
fn closed_shape_contracts_dispatch_builtins_natively_for_both_provenances() {
    for source in [
        "def run(h:{a:int})->int;h.size;end",
        "def run(h:{a:int})->int;h.size();end",
        "def run(h:{a:int})->int;h.length;end",
        "def run(h:{a:int})->bool;h.empty?;end",
        "def run(h:{a:int})->bool;h.nil?;end",
        "def run(h:{a:int})->bool;h.key?(:a);end",
        "def run(h:{a:int})->bool;h.has_key?('a');end",
        "def run(h:{a:int})->bool;h.include?(:missing);end",
        "def run(h:{a:int})->bool;h.member?('missing');end",
        "def run(h:{a:int});h.keys;end",
        "def run(h:{a:int})->array<int>;h.values;end",
        "def run(h:{a:int});h.to_a;end",
        "def run(h:{a:int})->int;h.a;end",
        "def run(h:{a:int})->int;h.fetch(:a);end",
        "def run(h:{a:int})->int;h.fetch('a');end",
        "def run(h:{a:int})->int;h.fetch(:zz,9);end",
        "def run(h:{a:int})->int;h.fetch(:zz) {|k| 9};end",
        "def run(h:{a:int})->int;n=0;h.each {|k,v| n+=v};n;end",
        "def run(h:{a:int})->array<int>;h.map {|k,v| v*2};end",
        "def run(h:{a:int});h.select {|k,v| v>0};end",
        "def run(h:{a:int});h.transform_values {|v| v+1};end",
        "def run(h:{a:int})->int;h.each_value {|v| v};h.size;end",
        "def run(h:{a:int});h.merge({b:2});end",
        "def run(h:{a:int})->int;h.merge({b:2}).size;end",
        "def run(h:{a:int});h.itself;end",
        "def run(h:{a:int});h.dup;end",
        "def run(h:{a:int});h.tap {|x| x};end",
        "def run(h:{a:int})->int;h.tap {|x| x}.size;end",
        "def run(h:{a:int})->int;h.yield_self {|x| x.size};end",
        "def run(h:{a:int})->bool;h.respond_to?(:size);end",
        "def run(h:{a:int})->int;h.send(:size);end",
        "def run(h:{a:int});h.public_send(:keys);end",
        "def run(h:{a:int})->int;h.send(:fetch,:a);end",
        "def run(h:{a:int})->int;n=0;h.send(:each) {|k,v| n+=v};n;end",
        "def run(h:{a:int})->int;h.size.to_s.size;end",
        "def run(h:{a:int})->int;[h][0].size;end",
        "def run(h:{a:int})->int;x={h:h};x.h.size;end",
        "def run(h:{a:int})->int;x={h:h};x[:h].fetch(:a);end",
        "def run(h:{a?:int})->int?;h.a;end",
        "def run(h:{a?:int})->int;h.size;end",
        "def run(h:{a?:int})->int;h.fetch(:a,0);end",
    ] {
        for input in both() {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
}

#[test]
fn general_and_open_contracts_keep_native_results_and_possible_overrides() {
    for source in [
        "def run(h:hash<string,int>)->int;h.size;end",
        "def run(h:hash<string,int>)->int;h.size();end",
        "def run(h:hash<string,int>)->array<string>;h.keys();end",
        "def run(h:hash<string,int>)->array<int>;h.values();end",
        "def run(h:hash<string,int>)->int;n=0;h.each {|k,v| n+=v};n;end",
        "def run(h:hash<string,int>)->array<int>;h.map {|k,v| v};end",
        "def run(h:hash<string,int>)->int;h.fetch('a');end",
        "def run(h:hash<string,int>)->int;h.fetch('zz') {7};end",
        "def run(h:hash<string,int>);h.fetch_values('a') {7};end",
        "def run(h:hash<string,int>);h.transform_values {|n| n+1};end",
        "def run(h:hash<string,int>);h.tap {|n| n};end",
        "def run(h:hash<string,int>)->int;h.send(:size);end",
        "def run(h:hash<string,int>)->bool;h.nil?;end",
        "def run(h:{a:int,...})->int;h.size;end",
        "def run(h:{a:int,...})->int;n=0;h.each {|k,v| n+=1};n;end",
        "def run(h:{a:int,...});h.store(:key,7);end",
        "def run(h:{a:int,...})->bool;h.empty?;end",
        "def run(h:{a:int,...})->int;h.size;end",
        "def run(h:{a:int,...})->int;h.a;end",
        "def run(h:{a:int,...})->int;n=0;h.each {|k,v| n+=1};n;end",
        "def run(h:{a:int,...});h.keys;end",
        "def run(h:{a:int,...})->int;h.fetch(:a);end",
        "def run(h:{a:int,...});h.merge({b:2});end",
        "def run(h:{a:int,...});h.tap {|x| x};end",
    ] {
        for input in both() {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
    // A general contract does not prove that a member key is absent, so a
    // possible override of a non-callable stored value is an ordinary error
    // path rather than a diagnostic.
    for (source, message) in [
        (
            "def run(h:hash<string,int>);h.size();end",
            "attempted to call non-callable value",
        ),
        (
            "def run(h:hash<string,int>);h.each {|k,v| v};end",
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{a:int,...});h.each {|k,v| v};end",
            "attempted to call non-callable value",
        ),
    ] {
        let colliding = Value::object(vec![
            (b"a".to_vec(), Value::int(1)),
            (b"size".to_vec(), Value::int(7)),
            (b"each".to_vec(), Value::int(7)),
        ]);
        assert_eq!(possible_failure(source, &[colliding]), message, "{source}");
        for input in both() {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
    // Members absent from an open shape or general hash fail without proving it.
    for source in [
        "def run(h:{a:int,...});h.b;end",
        "def run(h:hash<string,int>);h.b;end",
        "def run(h:{a:int,...});h.b(7);end",
    ] {
        for input in both() {
            assert_eq!(
                possible_failure(source, &[input]),
                "unknown hash method b (did you mean \"a\"?)",
                "{source}"
            );
        }
    }
}

#[test]
fn known_collisions_and_misses_remain_diagnostics() {
    // Each rejected script fails at runtime for at least one admitted provenance.
    for (source, input, message) in [
        (
            "def run(h:{size:string})->int;h.size;end",
            Value::object(vec![(b"size".to_vec(), Value::bytes(b"x".to_vec()))]),
            "expected int",
        ),
        (
            "def run(h:{size:int});h.size();end",
            Value::object(vec![(b"size".to_vec(), Value::int(7))]),
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{each:int});h.each {|k,v| v};end",
            Value::object(vec![(b"each".to_vec(), Value::int(7))]),
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{a:int});h.a();end",
            plain(),
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{a:int});h.b;end",
            plain(),
            "unknown hash method b",
        ),
        (
            "def run(h:{a:int});h.b;end",
            object(),
            "unknown hash method b",
        ),
        (
            "def run(h:{a:int});h::a;end",
            plain(),
            "scoped member access is only supported on enums and namespaces",
        ),
        (
            "def run(h:{a:int});h::size;end",
            object(),
            "unknown member size",
        ),
        (
            "def run(h:{a:int,...});h::b;end",
            plain(),
            "scoped member access is only supported on enums and namespaces",
        ),
        (
            "def run(h:{a:int,...});h::b;end",
            object(),
            "unknown member b",
        ),
        (
            "def run(h:{a:int,...});h::nil?;end",
            plain(),
            "scoped member access is only supported on enums and namespaces",
        ),
        (
            "def run(h:{a:int});h.send(:a);end",
            object(),
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{tap:int});h.tap {|x| x};end",
            Value::hash(vec![(b"tap".to_vec(), Value::int(7))]),
            "attempted to call non-callable value",
        ),
        (
            "def run(h:{a:int})->string;h.size;end",
            plain(),
            "expected string",
        ),
        (
            "def run(h:hash<string,int>)->string;h.fetch('a');end",
            plain(),
            "expected string",
        ),
        (
            "def run(h:hash<string,int>);h.clear.size;end",
            Value::object(vec![(b"clear".to_vec(), Value::int(7))]),
            "unknown int method size",
        ),
    ] {
        let error = runtime_error(source, &[input]);
        assert!(error.contains(message), "{source}: {error}");
        check(source, true);
    }
    // Plain-hash builtin names ignore stored fields; only an object field overrides.
    let sized = Value::hash(vec![(b"size".to_vec(), Value::int(9))]);
    assert_eq!(
        inferred_runtime("def run(h:{size:int})->int;h.size;end", &[sized], false).as_int(),
        Some(1)
    );
    let sized = Value::object(vec![(b"size".to_vec(), Value::int(9))]);
    assert_eq!(
        inferred_runtime("def run(h:{size:int})->int;h.size;end", &[sized], false).as_int(),
        Some(9)
    );
    // The scope provenance split is exact: a matching object namespace passes.
    check("def run(h:{a:int})->int;h::a;end", true);
    let outcome = Engine::new()
        .compile("def run(h:{a:int})->int;h::a;end")
        .unwrap()
        .call("run", &[object()], CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(1));
}

#[test]
fn optional_builtin_name_fields_split_present_and_absent_object_paths() {
    let sized = |value: Value| Value::object(vec![(b"size".to_vec(), value)]);
    // A present optional field overrides the builtin only on objects; the
    // absent path and the plain path stay native, so `int` is proven for both.
    for (input, expected) in [
        (Value::hash(vec![(b"size".to_vec(), Value::int(9))]), 1),
        (sized(Value::int(9)), 9),
        (Value::object(vec![]), 0),
    ] {
        let value = inferred_runtime("def run(h:{size?:int})->int;h.size;end", &[input], false);
        assert_eq!(value.as_int(), Some(expected));
    }
    // A present field of the wrong type is a known bad return arm even though
    // the key may be absent.
    let source = "def run(h:{size?:string})->int;h.size;end";
    assert!(runtime_error(source, &[sized(Value::bytes(b"x".to_vec()))]).contains("expected int"));
    assert_eq!(
        inferred_runtime(source, &[Value::object(vec![])], true).as_int(),
        Some(0)
    );
    check(source, true);
    // The finite present-field alternative remains a known contradiction even
    // though the absent-field alternative dispatches natively.
    for (source, name) in [
        ("def run(h:{size?:int})->int;h.size();end", &b"size"[..]),
        (
            "def run(h:{each?:int})->int;n=0;h.each {|k,v| n+=1};n;end",
            &b"each"[..],
        ),
    ] {
        let colliding = Value::object(vec![(name.to_vec(), Value::int(7))]);
        assert_eq!(
            runtime_error(source, &[colliding]),
            "attempted to call non-callable value",
            "{source}"
        );
        let plain = Value::hash(vec![(name.to_vec(), Value::int(7))]);
        assert_eq!(
            inferred_runtime(source, &[plain], true).as_int(),
            Some(1),
            "{source}"
        );
        assert_eq!(
            inferred_runtime(source, &[Value::object(vec![])], true).as_int(),
            Some(0),
            "{source}"
        );
    }
}

#[test]
fn general_hash_members_keep_the_declared_value_type_when_present() {
    // Both provenances read a present non-builtin key as the declared value
    // type, and an absent key fails; no path can yield a string.
    let present = [
        Value::hash(vec![
            (b"a".to_vec(), Value::int(1)),
            (b"b".to_vec(), Value::int(5)),
        ]),
        Value::object(vec![
            (b"a".to_vec(), Value::int(1)),
            (b"b".to_vec(), Value::int(5)),
        ]),
    ];
    for source in [
        "def run(h:hash<string,int>)->string;h.b;end",
        "def run(h:hash<string,int>)->string;h.fetch('b');end",
        "def run(h:{a:int,b?:int})->string;h.b;end",
    ] {
        for input in &present {
            let error = runtime_error(source, std::slice::from_ref(input));
            assert!(error.contains("expected string"), "{source}: {error}");
        }
        let script = Engine::new().compile(source).unwrap();
        let missing = both();
        for (input, absent) in present
            .iter()
            .map(|input| (input, false))
            .chain(missing.iter().map(|input| (input, true)))
        {
            let exact = script
                .check_call("run", std::slice::from_ref(input), &CallOptions::default())
                .unwrap();
            assert!(exact.incomplete.is_empty(), "{source}: {exact:?}");
            // `fetch` raises its documented miss before a result reaches the
            // return contract, while `h.b` reads an unknown member.
            let raises = absent && source.contains("fetch");
            assert_eq!(exact.diagnostics.is_empty(), raises, "{source}: {exact:?}");
            if raises {
                let error = runtime_error(source, std::slice::from_ref(input));
                assert!(error.contains("not found"), "{source}: {error}");
            }
        }
        check(source, true);
    }
    for source in [
        "def run(h:hash<string,int>)->int;h.b;end",
        "def run(h:hash<string,int>)->int;h.b + 1;end",
        "def run(h:{a:int,b?:int})->int;h.b + 1;end",
    ] {
        for input in &present {
            let value = inferred_runtime(source, std::slice::from_ref(input), false);
            assert!(matches!(value.as_int(), Some(5 | 6)), "{source}: {value}");
        }
        for input in both() {
            assert_eq!(
                possible_failure(source, &[input]),
                "unknown hash method b (did you mean \"a\"?)",
                "{source}"
            );
        }
    }
}

fn counting_object(counter: &Arc<AtomicUsize>, rounds: usize) -> Value {
    let each = {
        let counter = counter.clone();
        HostMethod::new_with_block("each", move |call, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            for i in 0..rounds {
                call.call_block(&[Value::bytes(b"k".to_vec()), Value::int(i as i64)])?;
            }
            Ok(Value::int(13))
        })
    };
    let size = {
        let counter = counter.clone();
        HostMethod::new("size", move |_, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Value::int(99))
        })
    };
    let fetch = {
        let counter = counter.clone();
        HostMethod::new("fetch", move |_, args, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Value::int(args.len() as i64 + 41))
        })
    };
    Value::object(vec![
        (b"a".to_vec(), Value::int(1)),
        (b"each".to_vec(), each.value()),
        (b"size".to_vec(), size.value()),
        (b"fetch".to_vec(), fetch.value()),
    ])
}

#[test]
fn callable_overrides_are_analyzed_without_executing_callbacks() {
    for rounds in [0, 1, 3] {
        for (source, expected, rejected) in [
            (
                "def run(h:{a:int,...});n=0;h.each {|k,v| n+=1};n;end",
                rounds as i64,
                false,
            ),
            (
                "def run(h:{a:int,...});n=0;r=h.each {|k,v| n+=v};[r,n];end",
                (0..rounds as i64).sum(),
                false,
            ),
            (
                "def run(h:{a:int,...});n=0;h.each {|k,v| n+=1;break 7};n;end",
                rounds.min(1) as i64,
                false,
            ),
            ("def run(h:{a:int,...})->int;h.size();end", 99, false),
            ("def run(h:{a:int,...})->int;h.fetch(:x);end", 42, false),
            ("def run(h:{a:int,...})->int;h.send(:size);end", 99, false),
            ("def run(h:{a:int,...})->int;h.size;end", 99, false),
        ] {
            let counter = Arc::new(AtomicUsize::new(0));
            let input = counting_object(&counter, rounds);
            let script = Engine::new().compile(source).unwrap();
            let options = CallOptions::default();
            let exact = script
                .check_call("run", std::slice::from_ref(&input), &options)
                .unwrap();
            let general = script.check_function("run", &options).unwrap();
            assert_eq!(
                counter.load(Ordering::SeqCst),
                0,
                "{source}: checking executed a callback"
            );
            assert!(exact.incomplete.is_empty(), "{source}: {exact:?}");
            assert!(general.is_clean(), "{source}: {general:?}");
            assert_eq!(
                !exact.diagnostics.is_empty(),
                rejected,
                "{source}: {exact:?}"
            );
            let result = script.call("run", &[input], options);
            if rejected {
                // A bare read of a host method is the attached-method error.
                assert!(result.is_err(), "{source}");
                continue;
            }
            let value = result
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .value;
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "{source}: runtime callback count"
            );
            let observed = match value.as_array() {
                Some(items) => {
                    // The host method's own result reaches the script unchanged.
                    assert_eq!(items[0].as_int(), Some(13), "{source}");
                    items[1].as_int()
                }
                None => value.as_int(),
            };
            assert_eq!(observed, Some(expected), "{source}");
        }
    }
}

#[test]
fn lookup_failures_and_argument_effects_keep_runtime_order() {
    for source in [
        // A missing non-builtin member fails before its arguments.
        "def run(h:{a:int});seen=[];begin;h.b(seen.push(1));rescue;nil;end;seen;end",
        // Native members and scoped calls evaluate arguments first.
        "def run(h:{a:int});seen=[];begin;h.size(seen.push(1));rescue;nil;end;seen;end",
        "def run(h:{a:int});seen=[];begin;h::size(seen.push(1));rescue;nil;end;seen;end",
        "def run(h:{a:int});seen=[];begin;h.a(seen.push(1));rescue;nil;end;seen;end",
    ] {
        for input in both() {
            let value = inferred_runtime(source, std::slice::from_ref(&input), true);
            let expected = if source.contains("h.b(") { "[]" } else { "[1]" };
            assert_eq!(value.to_string(), expected, "{source}");
        }
    }
    for source in [
        "def run(h:{a:int,...});seen=[];begin;h.b(seen.push(1));rescue;nil;end;seen;end",
        "def run(h:hash<string,int>);seen=[];begin;h.b(seen.push(1));rescue;nil;end;seen;end",
    ] {
        for input in both() {
            let value = inferred_runtime(
                source,
                std::slice::from_ref(&input),
                source.contains("hash<string,int>"),
            );
            assert_eq!(value.to_string(), "[]", "{source}");
        }
    }
    // Missing members are diagnosed even when execution rescues them.
    check("def run(h:{a:int})->int;begin;h.b;rescue;7;end;end", true);
}

#[test]
fn addressed_and_forwarded_mutations_keep_provenance_and_pending_parents() {
    for source in [
        "def run(h:{a:int});h.store(:b,2);h;end",
        "def run(h:{a:int});h.delete(:a);h;end",
        "def run(h:{a:int});h.clear;h;end",
        "def run(h:{a:int});h.replace({z:1});h;end",
        "def run(h:{a:int})->int;h.store(:b,2);h.size;end",
        "def run(h:{a:int});h.send(:store,:b,2);h;end",
        "def run(h:{a:int});h.public_send(:delete,:a);h;end",
        "def run(h:{a:int});a=[h];a[0].store(:b,2);a;end",
        "def run(h:{a:int})->int;a=[h];a[0].size;end",
        "def run(h:{a:int});x={h:h};x.h.store(:b,2);x;end",
        "def run(h:{a:int});x={h:h};x.h.delete(:a);x;end",
        "def run(h:{a:int})->int;x={h:h};x.h.fetch(:a);end",
        "def run(h:{a:int});h.a += 3;h;end",
        "def run(h:{a:int});h.size = 3;h;end",
        "def run(h:hash<string,int>);h.store('b',2);h;end",
        "def run(h:hash<string,int>);h.delete('a');h;end",
        "def run(h:hash<string,int>);h.clear;h;end",
        "def run(h:{a:int,...});h.store(:b,2);h;end",
        "def run(h:{a:int,...});h.delete(:a);h;end",
        "def run(h:{a:int});h.delete_if {|k,v| v>0};h;end",
        "def run(h:hash<string,int>);h.keep_if {|k,v| v>0};h;end",
        // Argument effects change the parent after the target element is selected.
        "def run(h:{a:int});a=[h];a[0].store(:b,begin;a.push({});2;end);a;end",
        "def run(h:{a:int});a=[h];a[-1].store(:b,begin;a.push({c:1});2;end);a;end",
        "def run(h:{a:int});x={h:h};x.h.store(:b,begin;x.store(:h,{q:1});2;end);x;end",
        "def run(h:{a:int});a=[h];a[0].delete(begin;a.push({});:a;end);a;end",
    ] {
        for input in both() {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
    // Object provenance survives mutation while plain data stays plain.
    for (source, expected) in [
        ("def run(h:{a:int});h.store(:size,9);h.size;end", [2, 9]),
        (
            "def run(h:{a:int});h.clear;h.store(:size,9);h.size;end",
            [1, 9],
        ),
        ("def run(h:{a:int});h.replace({size:9});h.size;end", [1, 9]),
    ] {
        for (input, expected) in both().into_iter().zip(expected) {
            let value = inferred_runtime(source, &[input], false);
            assert_eq!(value.as_int(), Some(expected), "{source}");
        }
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    for source in [
        "def run(h:hash<string,int>)->int;n=0;h.each {|k,v| n+=v};h.store('b',2);n+h.size;end",
        "def run(h:{a:int,...});a=[h];a[0].store(:b,begin;a.push({});2;end);a[0].tap {|x| x};end",
        "def run(h:{a:int})->int;begin;h::a;rescue;h.send(:size);end;end",
    ] {
        let mut facts = Facts::new(ctx)?;
        let report = analyze(ctx, &mut facts, source)?;
        assert!(report.incomplete.data.is_empty(), "{report:?}");
    }
    // A contract that may admit a protected match keeps the runtime failure of
    // the nested write as an error path without a static verdict.
    let mut facts = Facts::new(ctx)?;
    let report = analyze(
        ctx,
        &mut facts,
        "def run(h:{captures:array<string?>,...});h.captures.push('x');end",
    )?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn hash_dispatch_is_metered_and_releases_interrupted_analysis() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
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
        assert_eq!(result.as_ref().err().map(|error| error.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for steps in (0..stats.steps).step_by(307) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(307) {
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
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        assert_eq!(
            accounting(&mut ctx).unwrap_err().kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn hash_membership_checks_stored_keys_without_capture_fallback() {
    for (source, expected, rejected) in [
        (
            "h={x:nil};[h.key?(:x),h.has_key?('x'),h.include?(:x),h.member?('x'),h.key?(:y)]",
            "[true, true, true, true, false]",
            false,
        ),
        (
            "m='a'.match('(?<x>a)');[m.key?(:x),m[:x],m.named_captures.key?(:x)]",
            "[false, a, true]",
            false,
        ),
        ("begin;{}.key?(1);rescue RuntimeError;7;end", "7", true),
        ("begin;{}.has_key?;rescue RuntimeError;7;end", "7", true),
    ] {
        super::scope_tests::top(source, expected, rejected);
    }
    for source in [
        "def run(h:{x:nil|int})->int;if h.key?(:x);0;else;'bad';end;end",
        "def run(h:{x?:nil|int})->bool;h.key?(:x);end",
        "def run(h:{x:int})->int;if h.key?(:y);'bad';else;0;end;end",
    ] {
        check(source, false);
    }
}
