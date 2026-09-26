use super::integers::{Bounds, Comparison};
use crate::{CallOptions, CheckedOutcome, Engine, HostMethod, Signature, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn clean(source: &str, expected: i64) {
    let script = Engine::legacy_unchecked().compile(source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
    let CheckedOutcome::Executed(result) = script
        .checked_call("run", &[], CallOptions::default())
        .unwrap()
    else {
        panic!("{source}")
    };
    assert_eq!(result.value.as_int(), Some(expected), "{source}");
}

fn rejected(source: &str) {
    let script = Engine::legacy_unchecked().compile(source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
}

#[test]
fn bounded_integer_loops_keep_array_reads_inside_both_endpoints() {
    for body in [
        "i=0;while i<a.length;n+=a[i];i+=1;end",
        "i=0;until i>=a.size;n+=a[i];i+=1;end",
        "i=0;while a.length>i;n+=a[i];i+=1;end",
        "i=0;while i<=a.length-1;n+=a[i];i+=1;end",
        "i=a.length-1;while i>=0;n+=a[i];i-=1;end",
        "i=-a.length;while i<0;n+=a[i];i+=1;end",
    ] {
        clean(&format!("def run;a=[10,20,30];n=0;{body};n;end"), 60);
    }
    clean(
        "def run;a=[];n=0;i=0;while i<a.length;n+=a[i];i+=1;end;n;end",
        0,
    );
}

#[test]
fn integer_guards_refine_unknown_host_results_without_executing_them() {
    for (guard, negative) in [
        ("i>=0 && i<a.length", false),
        ("!(i<0 || i>=a.length)", false),
        ("i>=0 && i<=3 && i != 3", false),
        ("3>i && 0<=i", false),
        ("i>=-3 && i<0", true),
        ("i>=-4 && i<0 && i != -4", true),
    ] {
        for input in -5..=5 {
            let count = Arc::new(AtomicUsize::new(0));
            let seen = count.clone();
            let method = HostMethod::new("choose", move |_, _, _| {
                seen.fetch_add(1, Ordering::Relaxed);
                Ok(Value::int(input))
            })
            .with_signature(Signature {
                params: vec![],
                result: "int".into(),
                accepts_block: false,
            })
            .unwrap();
            let mut engine = Engine::legacy_unchecked();
            engine.register_method("choose", method);
            let source =
                format!("def run;a=[10,20,30];i=choose();if {guard};a[i]+1;else;0;end;end");
            let script = engine.compile(&source).unwrap();
            let report = script
                .check_call("run", &[], &CallOptions::default())
                .unwrap();
            assert!(report.is_clean(), "{guard}: {report:?}");
            assert_eq!(count.load(Ordering::Relaxed), 0);
            let result = script.call("run", &[], CallOptions::default()).unwrap();
            let index = if negative { input + 3 } else { input };
            let expected = if (0..3).contains(&index) {
                (index + 1) * 10 + 1
            } else {
                0
            };
            assert_eq!(result.value.as_int(), Some(expected));
            assert_eq!(count.load(Ordering::Relaxed), 1);
        }
    }
}

#[test]
fn changed_indexes_and_receivers_invalidate_old_bounds() {
    for source in [
        "def run;a=[1,2];i=0;if i<a.length;a.clear;a[i]+1;end;end",
        "def run;a=[1,2];i=0;if i<a.length;i=7;a[i]+1;end;end",
        "def run;a=[1,2];i=0;if i<(begin;i=7;2;end);a[i]+1;end;end",
        "def run;a=[1,2];i=0;if i<2 && (begin;i=7;true;end);a[i]+1;end;end",
        "def run;a=[1,2];i=0;j=0;if i<2;j+=7;a[j]+1;end;end",
        "def run;a=[1,2];i=0;n=0;while i<=a.length;n+=a[i];i+=1;end;n;end",
        "def run;a=[1,2];i=-3;n=0;while i<0;n+=a[i];i+=1;end;n;end",
    ] {
        rejected(source);
    }
    clean(
        "def run;a=[1,2];i=0;if i<a.length;a[(begin;a.clear;i;end)]+1;end;end",
        2,
    );
}

#[test]
fn integer_arithmetic_and_calls_preserve_selected_element_types() {
    clean(
        "def get(a,i);a[i]+1;end;def run;a=['bad',7,'bad'];get(a,(0+1)*1);end",
        8,
    );
    clean("def run;i=3;while i>1;i-=1;end;['bad',7,'bad'][i]+1;end", 8);
    clean(
        "def run;n=0;while n<3;n+=1;end;if n<0;'bad'+false;end;7;end",
        7,
    );
    rejected("def run;['bad',7][1+1]+1;end");
}

#[test]
fn retries_and_nested_integer_updates_converge_with_fixed_source_thresholds() {
    clean(
        "def once;yield;end;def run;n=0;begin;once { n+=1;if n<3;raise 'retry';end };rescue;retry;end;n;end",
        3,
    );
    clean("def run;a=[0];i=0;while i<4;a[0]+=1;i+=1;end;a[0];end", 4);
    clean(
        "def run;a={n:0};i=0;while i<4;a[:n]+=2;i+=1;end;a[:n];end",
        8,
    );
    clean(
        "def once;yield;end;def run;n=0;i=0;while i<4;once { n+=i };i+=1;end;n;end",
        6,
    );
    rejected("def once;yield;end;def run;a=[1,2];i=0;if i<2;once { i+=7 };a[i]+1;end;end");
}

#[test]
fn bounded_integers_remain_valid_native_receivers_and_lookup_keys() {
    for body in [
        "n=0;i.times { n+=1 };n",
        "[10,20,30].fetch(i) { 0 }+1",
        "n=0;i.upto(3) { n+=1 };n",
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let method = HostMethod::new("choose", move |_, _, _| {
            seen.fetch_add(1, Ordering::Relaxed);
            Ok(Value::int(1))
        })
        .with_signature(Signature {
            params: vec![],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap();
        let mut engine = Engine::legacy_unchecked();
        engine.register_method("choose", method);
        let source = format!("def run;i=choose();if i>=0 && i<3;{body};else;0;end;end");
        let script = engine.compile(&source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert_eq!(count.load(Ordering::Relaxed), 0);
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        let expected = if body.contains("times") {
            1
        } else if body.contains("fetch") {
            21
        } else {
            3
        };
        assert_eq!(result.value.as_int(), Some(expected));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn integer_case_bounds_preserve_possible_float_matches() {
    let mut engine = Engine::legacy_unchecked();
    engine.register_method(
        "unknown",
        HostMethod::new("unknown", |_, _, _| Ok(Value::float(1.0))),
    );
    engine.register_method(
        "index",
        HostMethod::new("index", |_, _, _| Ok(Value::int(1)))
            .with_signature(Signature {
                params: vec![],
                result: "int".into(),
                accepts_block: false,
            })
            .unwrap(),
    );
    let source = "def only_int(x:int);x;end;def run;i=index();if i>=0 && i<3;x=unknown();case x;when i;only_int(x);else;0;end;else;0;end;end";
    let script = engine.compile(source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(script.call("run", &[], CallOptions::default()).is_err());
}

#[test]
fn integer_bound_arithmetic_contains_every_small_concrete_result() {
    for min in -3..=3 {
        for max in min..=3 {
            let a = Bounds {
                min: Some(min),
                max: Some(max),
            };
            for other_min in -3..=3 {
                for other_max in other_min..=3 {
                    let b = Bounds {
                        min: Some(other_min),
                        max: Some(other_max),
                    };
                    for x in min..=max {
                        for y in other_min..=other_max {
                            for (op, expected) in [
                                ("+", i128::from(x) + i128::from(y)),
                                ("-", i128::from(x) - i128::from(y)),
                                ("*", i128::from(x) * i128::from(y)),
                            ] {
                                assert!(
                                    a.arithmetic(op, b).unwrap().includes(expected),
                                    "{a:?} {op} {b:?}: {x}, {y}"
                                );
                            }
                            for (comparison, expected) in [
                                (Comparison::Less, x < y),
                                (Comparison::LessEqual, x <= y),
                                (Comparison::Greater, x > y),
                                (Comparison::GreaterEqual, x >= y),
                                (Comparison::Equal, x == y),
                                (Comparison::NotEqual, x != y),
                            ] {
                                if let Some(actual) = a.compare(comparison, b) {
                                    assert_eq!(actual, expected, "{a:?} {comparison:?} {b:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn integer_bounds_round_overflow_outward_and_widen_expanding_loops() {
    for x in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
        let a = Bounds::point(x);
        assert!(a.negate().includes(-i128::from(x)));
        for y in [i64::MIN, -1, 0, 1, i64::MAX] {
            let b = Bounds::point(y);
            for (op, expected) in [
                ("+", i128::from(x) + i128::from(y)),
                ("-", i128::from(x) - i128::from(y)),
                ("*", i128::from(x) * i128::from(y)),
            ] {
                assert!(
                    a.arithmetic(op, b).unwrap().includes(expected),
                    "{x} {op} {y}"
                );
            }
            let widened = a.widen(b);
            assert!(widened.contains(a));
            assert!(widened.contains(b));
            assert_eq!(widened.widen(b), widened);
        }
    }
    clean(
        "def run;n=9223372036854775807;n+=1;if n<0;'bad'+false;end;7;end",
        7,
    );
    clean(
        "def run;n=-9223372036854775808;n-=1;if n>0;'bad'+false;end;7;end",
        7,
    );
}

#[test]
fn integer_range_analysis_is_metered_interruptible_and_reclaims_storage() {
    super::namespace_tests::metered(
        "def run;a=[1,2,3];n=0;i=0;while i<a.length;n+=a[i];i+=1;end;n;end",
    );
    super::namespace_tests::metered(
        "def run;i=3;while i>=0;begin;i-=1;next if i==2;ensure;[1,2,3].length;end;end;i;end",
    );
}
