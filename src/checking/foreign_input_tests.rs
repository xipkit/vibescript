use super::{entry, whole};
use crate::{CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Script};

fn check(ctx: &mut CallContext, script: &Script, options: &CallOptions, all: bool) -> Result<()> {
    if all {
        let report = whole::check(ctx, script, options)?;
        assert!(report.is_clean(), "{report:?}");
    } else {
        let checked = entry::check_function(ctx, script, "run", options)?;
        assert!(checked.analysis.issues.data.is_empty(), "{checked:?}");
        assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    }
    Ok(())
}

#[test]
fn foreign_input_domains_and_constructor_jobs_obey_limits_and_release_storage() {
    let source = Engine::legacy_unchecked()
        .compile("class Box;property n:int;def initialize(n:int=7);@n=n;end;def answer;7;end;end;def klass;Box;end")
        .unwrap();
    let class = source
        .call("klass", &[], CallOptions::default())
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    let receiver = Engine::legacy_unchecked().compile("def run(x:Foreign,others:array<Foreign>)->array<int>;x.n=7;others.map{|item|item.n=7;item.answer};end").unwrap();
    for all in [false, true] {
        let mut ctx = CallContext::new(options.clone());
        check(&mut ctx, &receiver, &options, all).unwrap();
        let stats = ctx.stats();
        assert_eq!(stats.retained_memory_bytes, 0);
        let mut again = CallContext::new(options.clone());
        check(&mut again, &receiver, &options, all).unwrap();
        assert_eq!(again.stats().steps, stats.steps);
        assert_eq!(again.stats().peak_memory_bytes, stats.peak_memory_bytes);
        assert_eq!(again.stats().retained_memory_bytes, 0);
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            let needed = if kind == ErrorKind::Steps {
                stats.steps as usize
            } else {
                stats.peak_memory_bytes
            };
            for limit in [
                0,
                1,
                needed / 4,
                needed / 2,
                needed * 3 / 4,
                needed - 1,
                needed,
            ] {
                let limited = CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(limit as u64),
                        memory_bytes: (kind == ErrorKind::Memory).then_some(limit),
                        ..Limits::default()
                    },
                    ..options.clone()
                };
                let mut ctx = CallContext::new(limited.clone());
                let result = check(&mut ctx, &receiver, &limited, all);
                if limit == needed {
                    result.unwrap();
                } else {
                    assert_eq!(result.unwrap_err().kind, kind);
                    assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
                }
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
        for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut options = options.clone();
            if kind == ErrorKind::Cancelled {
                options.cancellation = Default::default();
                options.cancellation.cancel();
            } else {
                options.deadline = Some(std::time::Instant::now());
            }
            let mut ctx = CallContext::new(options.clone());
            assert_eq!(
                check(&mut ctx, &receiver, &options, all).unwrap_err().kind,
                kind
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
