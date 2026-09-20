use super::{collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime};
use crate::{CallContext, CallOptions, ErrorKind, HostMethod, Limits, Result, Value};

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

#[test]
fn unknown_calls_remain_gradual_without_hiding_known_contradictions() {
    for source in [
        "def run(value);value.foo;end",
        "def run(value:any);value.foo(7,named:8);end",
        "def run(value);value.foo.bar;end",
        "def run(value);value::foo;end",
        "def run(value);value::foo(7);end",
        "def run(value);value(7);end",
        "def take(n:int);n;end;def run(value)->int;take(value.foo);end",
        "def run(value);n=0;value.visit {|item|n+=1};n;end",
        "def run(value);n=0;value {|item|n+=1};n;end",
        "def run(value);value.map {|item|item.foo};end",
        "def run(value);value.visit {value.visit {7}};end",
        "def run(value);value.nil?;end",
        "def run(value);value.push(7);value;end",
        "def run(value);value.push(named:7);end",
        "def run(value);n=0;value.delete_if {n+=1};n;end",
        "def run(value);value.clear.foo;end",
        "def run(value);for item in [value];item.foo;end;end",
        "def run(value);[1].group_by {value.send(:size) {7}};end",
        "def run(xs);xs.group_by {:a};end",
        "def run(value);value.tap {|n|n};end",
        "def run(xs);xs.grep(7) {|n|n};end",
        "def run(xs:array);for x in xs;x.no_such_method;end;end",
        "def run(value);value.send(:size);end",
        "def run(value);value.send(:size) {7};end",
        "def run(value);[value,2].reduce('custom');end",
        "def run(value)->int;n=7;value.foo;n;end",
    ] {
        check(source, false);
    }
    for source in [
        "def run(value,flag:bool);(if flag;7;else;value;end).missing;end",
        "def take(n:int);n;end;def run(value);value.visit {take('bad')};end",
        "def run(value)->int;value.visit {return 'bad'};7;end",
        "def run(value)->int;value.visit {break 'bad'};end",
        "def take(n:int);n;end;def run(value);value.foo(take('bad'));end",
        "def take(n:int);n;end;def run(value);value.delete_if {take('bad')};end",
        "def run(value,flag:bool);(if flag;7;else;value;end)(8);end",
        "def run(value,flag:bool);f=if flag;7;else;value;end;f(8);end",
    ] {
        check(source, true);
    }
}

#[test]
fn unknown_call_effects_preserve_known_namespace_contradictions() {
    let namespace =
        "module M;@value=7;def self.get;@value;end;def self.set(value);@value=value;end;end;";
    for body in [
        "M.set('bad');value.foo;M.get",
        "value.visit {M.set('bad')};M.get",
        "begin;value.visit {M.set('bad');raise 'stop'};rescue;nil;end;M.get",
    ] {
        check(&format!("{namespace}def run(value)->int;{body};end"), true);
    }
}

#[test]
fn unknown_mutating_callbacks_keep_pending_parent_writes_conservative() {
    for input in [
        Value::array(vec![]),
        Value::array(vec![Value::int(7)]),
        Value::array(vec![Value::int(7), Value::int(8)]),
    ] {
        for source in [
            "def run(value);value.delete_if {false};value;end",
            "def run(value);n=0;value.delete_if {n+=1;false};n;end",
            "def run(value);value.delete_if {break 7};end",
            "def run(value);a=[value];a[-1].delete_if {a.push([]);false};a;end",
            "def run(value);a=[value];a[0].delete_if {a=[[]];false};a;end",
            "def run(value);a=[value];begin;a[0].delete_if {a=[[]];raise 'stop'};rescue;nil;end;a;end",
        ] {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
}

fn receiver(rounds: usize, arguments: Vec<Value>, swallow: bool) -> Value {
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        for _ in 0..rounds {
            match call.call_block(&arguments) {
                Ok(_) => (),
                Err(_) if swallow => (),
                Err(error) => return Err(error),
            }
        }
        Ok(Value::int(13))
    });
    Value::object(vec![(b"visit".to_vec(), method.value())])
}

#[test]
fn unknown_callback_schedules_cover_captures_control_and_argument_shapes() {
    for rounds in [0, 1, 3] {
        for args in [
            vec![],
            vec![Value::int(7)],
            vec![Value::int(7), Value::bytes(b"text".to_vec())],
            vec![Value::array(vec![Value::int(7), Value::int(8)])],
        ] {
            for swallow in [false, true] {
                let receiver = receiver(rounds, args.clone(), swallow);
                for source in [
                    "def run(value);n=0;r=value.visit {n+=1};[r,n];end",
                    "def run(value);a=[];value.visit {a.push(7)};a;end",
                    "def run(value);a=nil;value.visit {|x,y,z|a=[x,y,z]};a;end",
                    "def run(value);a=nil;value.visit {a=[_1,_2,_9]};a;end",
                    "def run(value);n=0;r=value.visit {n+=1;break 7};[r,n];end",
                    "def run(value);n=0;value.visit {n+=1;return n};n;end",
                    "def run(value);n=0;r=value.visit {n+=1;next 7};[r,n];end",
                    "def run(value);n=0;begin;value.visit {n+=1;raise 'stopped'};rescue;n+=10;end;n;end",
                ] {
                    inferred_runtime(source, std::slice::from_ref(&receiver), false);
                }
            }
        }
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    for source in [
        "def run(value);n=0;a=[];value.visit {|item|n+=1;a.push(item)};[n,a];end",
        "def run(value);a=[value];a[-1].delete_if {a.push([]);false};a;end",
    ] {
        let mut facts = Facts::new(ctx)?;
        let report = analyze(ctx, &mut facts, source)?;
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert!(report.issues.data.is_empty(), "{report:?}");
    }
    Ok(())
}

#[test]
fn dynamic_call_schedules_are_metered_and_release_failed_analysis() {
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
    for steps in (0..stats.steps).step_by(311) {
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
    for memory in (0..stats.peak_memory_bytes).step_by(311) {
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
