use super::*;
use crate::checking::{
    entry::{self, Call},
    facts::{Atom, Facts},
};
use crate::{CallOptions, Engine, ErrorKind, Limits};

#[test]
fn type_descriptions_bound_shared_graph_expansion_and_default_stack_use() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut deep = Atom::Int.fact();
    for _ in 0..2000 {
        deep = facts.array(&mut ctx, deep).unwrap();
    }
    let mut shared = Atom::String.fact();
    for _ in 0..32 {
        shared = facts.tuple(&mut ctx, &[shared; 16]).unwrap();
    }
    for fact in [deep, shared] {
        let mut writer = types::Writer::new(&mut ctx);
        writer.fact(&facts, fact).unwrap();
        let (text, charge) = writer.finish();
        assert!(text.len() <= 4096 && text.contains("..."), "{}", text.len());
        drop(text);
        drop(charge);
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn work(ctx: &mut CallContext, script: &crate::Script) -> crate::Result<CheckReport> {
    let checked = entry::check(
        ctx,
        Call {
            script,
            name: "run",
            arguments: &[],
            keywords: &[],
            options: &CallOptions::default(),
        },
    )?;
    let mut report = build(ctx, &script.inner.code.program, &checked)?;
    drop(checked);
    ctx.checkpoint()?;
    report.stats = ctx.stats();
    Ok(report)
}

#[test]
fn report_sorting_and_allocation_failures_release_all_temporary_storage() {
    let mut source = String::from("def run\n");
    for i in 0..48 {
        source.push_str(&format!("begin;{i}-\"bad\";rescue;nil;end\n"));
    }
    source.push_str("end");
    let script = Engine::new().compile(&source).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let report = work(&mut ctx, &script).unwrap();
    assert_eq!(report.diagnostics.len(), 48);
    assert!(
        report
            .diagnostics
            .windows(2)
            .all(|pair| pair[0].offset < pair[1].offset)
    );
    let stats = ctx.stats();
    assert!(stats.retained_memory_bytes > 0);
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for sample in 0..24 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let kind = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 24);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 24);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(work(&mut ctx, &script).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
