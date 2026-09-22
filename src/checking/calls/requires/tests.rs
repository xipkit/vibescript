use super::*;
use crate::checking::{entry, report};
use crate::{CallOptions, Engine, HostMethod, Limits, ModuleConfig, Script, Signature, Stats};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".cache/tmp")
            .join(format!(
                "require-accounting-{}-{}",
                crate::loading::test_support::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn check(ctx: &mut CallContext, script: &Script, options: &CallOptions) -> Result<Stats> {
    let result = (|| {
        let checked = entry::check(
            ctx,
            entry::Call {
                script,
                name: "run",
                arguments: &[],
                keywords: &[],
                options,
            },
        )?;
        let report = report::build(ctx, &script.inner.code.program, &checked)?;
        assert!(report.is_clean(), "{report:?}");
        drop(checked);
        ctx.checkpoint()?;
        Ok(ctx.stats())
    })();
    script.inner.loader.clear();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    result
}

#[test]
fn import_discovery_publication_and_retries_share_exact_limits_and_release_scratch() {
    let directory = Directory::new();
    fs::write(
        directory.0.join("inner.vibe"),
        "seed=10;def value(n:int=2)->int;seed+n;end",
    )
    .unwrap();
    fs::write(
        directory.0.join("outer.vibe"),
        "m=require('./inner');def value(n:int=2)->int;m.value(n);end",
    )
    .unwrap();
    fs::write(
        directory.0.join("branch.vibe"),
        "n||=0;n+=1;if flags[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    )
    .unwrap();
    fs::write(
        directory.0.join("objects.vibe"),
        "class Box;property value:int;def initialize(n:int=3);@value=n;end;def +(n:int)->int;@value+n;end;end;def box_class;Box;end;def receiving(x:Local)->int;x.value;end",
    )
    .unwrap();
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![directory.0.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    engine.register_method(
        "choose",
        HostMethod::new("choose", |_, _, _| {
            panic!("checker executed a host callback")
        })
        .with_signature(Signature {
            params: vec![],
            result: "bool".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    for source in [
        "def run;require(:outer,as: :M);require(:outer,as: :M);M.value();end",
        "def run;begin;require(:fail);rescue;nil;end;require(:fail).value();end",
        "def run;[0].each{if choose();flags[0]=true;begin;require(:branch);rescue;nil;end;end};flags[0]=false;require(:branch).value();end",
        "class Local;property value:int;def initialize;@value=4;end;end;def run->int;m=require(:objects);c=m.box_class();b=c.new();b.value+=2;if b+2==7;m.receiving(Local.new);else;false;end;end",
    ] {
        fs::write(
            directory.0.join("fail.vibe"),
            "state[0]+=1;if state[0]==1;raise 'retry';end;def value;state[0];end",
        )
        .unwrap();
        let script = engine.compile(source).unwrap();
        let supplied = CallOptions {
            globals: [
                ("state".into(), Value::array(vec![Value::int(0)])),
                ("flags".into(), Value::array(vec![Value::boolean(false)])),
            ]
            .into(),
            ..CallOptions::default()
        };
        let mut ctx = CallContext::new(supplied.clone());
        let stats = check(&mut ctx, &script, &supplied).unwrap();
        for _ in 0..3 {
            let mut ctx = CallContext::new(supplied.clone());
            let again = check(&mut ctx, &script, &supplied).unwrap();
            assert_eq!(again.steps, stats.steps);
            assert_eq!(again.peak_memory_bytes, stats.peak_memory_bytes);
        }
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            for sample in [0, 1, 4, 8, 12, 15, 16, 17] {
                let options = CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(if sample == 17 {
                            stats.steps - 1
                        } else {
                            stats.steps * sample / 16
                        }),
                        memory_bytes: (kind == ErrorKind::Memory).then_some(if sample == 17 {
                            stats.peak_memory_bytes - 1
                        } else {
                            stats.peak_memory_bytes * sample as usize / 16
                        }),
                        ..Limits::default()
                    },
                    ..supplied.clone()
                };
                let mut ctx = CallContext::new(options.clone());
                let result = check(&mut ctx, &script, &options);
                if sample == 16 {
                    assert!(result.is_ok(), "{result:?}");
                } else {
                    assert_eq!(result.unwrap_err().kind, kind);
                    assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
                }
            }
        }
    }
}
