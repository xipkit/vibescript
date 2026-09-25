use super::*;
use crate::checking::{
    entry::{self, Call},
    facts::{Atom, Facts},
};
use crate::{CallOptions, Engine, ErrorKind, Limits};
use std::sync::Arc;

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

fn named(source: &str, filename: Option<&[u8]>) -> Arc<crate::code::Code> {
    let mut program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
    program.source.filename = filename.map(Arc::from);
    assert!(program.namespaces.is_empty());
    Arc::new_cyclic(|owner| {
        program.owner = owner.clone();
        crate::code::Code {
            program,
            hosts: Vec::new(),
            origin: None,
            exports: Vec::new(),
            static_types: false,
        }
    })
}

fn sources(ctx: &mut CallContext, codes: &[Arc<crate::code::Code>]) -> crate::Result<CheckReport> {
    use crate::checking::{
        calls::{Analysis, LocatedIssue},
        flow::{Issue, IssueKind},
    };

    let mut facts = Facts::new(ctx)?;
    let mut analysis = Analysis {
        returns: Atom::Never.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: 0,
    };
    for code in codes {
        let owner = facts.source_owner(ctx, code, None)?;
        let source = facts.source_id(ctx, owner)?;
        for _ in 0..2 {
            for kind in [
                IssueKind::TypeBinding {
                    ty: 0,
                    ambiguous: false,
                },
                IssueKind::Member {
                    name: 0,
                    receiver: Atom::Int.fact(),
                    arguments: Atom::Nil.fact(),
                },
            ] {
                analysis.issues.push(
                    ctx,
                    LocatedIssue {
                        source,
                        function: 1,
                        issue: Issue { pc: 0, kind },
                    },
                )?;
            }
            analysis.incomplete.push(
                ctx,
                Location {
                    source,
                    function: 1,
                    pc: 0,
                },
            )?;
        }
    }
    let checked = Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    };
    // This program has neither the referenced function nor the type/member metadata.
    let root = crate::bytecode::compile("nil", Vec::new(), &()).unwrap();
    let mut report = build(ctx, &root, &checked)?;
    drop(checked);
    ctx.checkpoint()?;
    report.stats = ctx.stats();
    Ok(report)
}

#[test]
fn reports_use_defining_source_metadata_and_sort_by_filename() {
    let codes = [
        named("def zebra(x:Zebra)\n x.zed\nend", Some(b"z.vibe")),
        named("def alpha(x:Alpha)\n x.aye\nend", Some(b"a.vibe")),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let report = sources(&mut ctx, &codes).unwrap();
    assert_eq!(report.diagnostics.len(), 4);
    assert_eq!(report.incomplete.len(), 2);
    for (index, (code, function, annotation, member)) in [
        (&codes[1], "alpha", "Alpha", "aye"),
        (&codes[0], "zebra", "Zebra", "zed"),
    ]
    .into_iter()
    .enumerate()
    {
        let diagnostics = &report.diagnostics[index * 2..index * 2 + 2];
        assert!(
            diagnostics.iter().any(|d| d.message.contains(annotation)),
            "{report:?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.message.contains(member)),
            "{report:?}"
        );
        let body = &code.program.functions[1];
        let offset = body.locations[0];
        let expected = code.program.source.position(offset);
        for diagnostic in diagnostics.iter().chain([&report.incomplete[index]]) {
            assert_eq!(diagnostic.function, function);
            assert_eq!(diagnostic.filename, code.program.source.filename);
            assert_eq!(diagnostic.offset, offset as usize);
            assert_eq!(diagnostic.position.line, expected.line);
            assert_eq!(diagnostic.position.column, expected.column);
            assert_eq!(
                diagnostic.code_frame,
                code.program
                    .source
                    .frame_metered(&mut ctx, offset, expected)
                    .unwrap()
                    .0
            );
        }
    }
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn reports_keep_distinct_anonymous_and_same_named_sources_without_retaining_code() {
    let text = "def same(x:Missing);x.unknown;end";
    let codes = [
        named(text, None),
        named(text, Some(b"same.vibe")),
        named(text, None),
        named(text, Some(b"same.vibe")),
    ];
    let weak: Vec<_> = codes.iter().map(Arc::downgrade).collect();
    let mut ctx = CallContext::new(CallOptions::default());
    let report = sources(&mut ctx, &codes).unwrap();
    assert_eq!(report.diagnostics.len(), 8);
    assert_eq!(report.incomplete.len(), 4);
    assert!(report.incomplete[..2].iter().all(|d| d.filename.is_none()));
    assert!(
        report.incomplete[2..]
            .iter()
            .all(|d| d.filename.as_deref() == Some(b"same.vibe"))
    );
    assert_eq!(report.diagnostics[0].message, report.diagnostics[2].message);
    assert_eq!(report.diagnostics[4].message, report.diagnostics[6].message);
    assert_ne!(report.diagnostics[0]._source, report.diagnostics[2]._source);
    assert_ne!(report.diagnostics[4]._source, report.diagnostics[6]._source);
    drop(codes);
    assert!(weak.iter().all(|code| code.upgrade().is_none()));
    assert!(ctx.stats().retained_memory_bytes > 0);
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn multi_source_reports_share_exact_limits_and_release_interrupted_storage() {
    let text = "def same(x:Missing);x.unknown;end";
    let long = [b'p'; 4096];
    let codes = [
        named(text, None),
        named(text, Some(&long)),
        named(text, Some(&long)),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let report = sources(&mut ctx, &codes).unwrap();
    let stats = report.stats;
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in 0..=17 {
            let steps = if sample == 17 {
                stats.steps - 1
            } else {
                stats.steps * sample / 16
            };
            let memory = if sample == 17 {
                stats.peak_memory_bytes - 1
            } else {
                stats.peak_memory_bytes * sample as usize / 16
            };
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: (kind == ErrorKind::Steps).then_some(steps),
                    memory_bytes: (kind == ErrorKind::Memory).then_some(memory),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let result = sources(&mut ctx, &codes);
            if sample == 16 {
                drop(result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut options = CallOptions::default();
        if kind == ErrorKind::Cancelled {
            options.cancellation.cancel();
        } else {
            options.deadline = Some(std::time::Instant::now());
        }
        let mut ctx = CallContext::new(options);
        assert_eq!(sources(&mut ctx, &codes).unwrap_err().kind, kind);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn source_report_budgets_do_not_depend_on_compiled_addresses() {
    let mut expected = None;
    for _ in 0..16 {
        let codes = [
            named("def one(x:One);x.aye;end", Some(b"same.vibe")),
            named("def two(x:Two);x.bee;end", Some(b"same.vibe")),
        ];
        let mut ctx = CallContext::new(CallOptions::default());
        let report = sources(&mut ctx, &codes).unwrap();
        let actual = (
            report.stats.steps,
            report.stats.peak_memory_bytes,
            report.stats.retained_memory_bytes,
        );
        assert_eq!(actual, *expected.get_or_insert(actual));
        drop(report);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn call_report(
    ctx: &mut CallContext,
    caller: &Arc<crate::code::Code>,
    callee: &Arc<crate::code::Code>,
    target: impl FnOnce(crate::checking::sources::SourceId) -> crate::checking::calls::Target,
    failure: crate::checking::arguments::Failure,
) -> crate::Result<CheckReport> {
    use crate::checking::{
        calls::{Analysis, LocatedIssue},
        flow::{Issue, IssueKind},
    };
    let mut facts = Facts::new(ctx)?;
    let owner = facts.source_owner(ctx, caller, None)?;
    let source = facts.source_id(ctx, owner)?;
    let owner = facts.source_owner(ctx, callee, None)?;
    let target = target(facts.source_id(ctx, owner)?);
    let mut analysis = Analysis {
        returns: Atom::Never.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: 0,
    };
    analysis.issues.push(
        ctx,
        LocatedIssue {
            source,
            function: 1,
            issue: Issue {
                pc: 0,
                kind: IssueKind::Call { target, failure },
            },
        },
    )?;
    let checked = Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    };
    let mut report = build(ctx, &caller.program, &checked)?;
    drop(checked);
    ctx.checkpoint()?;
    report.stats = ctx.stats();
    Ok(report)
}

#[test]
fn cross_source_call_messages_use_callee_names_and_caller_locations() {
    use crate::checking::{arguments::Failure, calls::Target};
    let caller = named(
        "def caller(wrong); invoke(wrong); end",
        Some(b"caller.vibe"),
    );
    let callee = named("def callee(payload); payload; end", Some(b"library.vibe"));
    for kind in 0..3 {
        let mut ctx = CallContext::new(CallOptions::default());
        let report = call_report(
            &mut ctx,
            &caller,
            &callee,
            |source| {
                let function = source.callable(1);
                match kind {
                    0 => Target::Function(function),
                    1 => Target::Block(function),
                    _ => Target::Method {
                        function,
                        receiver: Atom::Int.fact(),
                        constructor: false,
                    },
                }
            },
            Failure::Missing(0),
        )
        .unwrap();
        assert_eq!(report.diagnostics.len(), 1);
        let diagnostic = &report.diagnostics[0];
        assert!(diagnostic.message.contains("callee"), "{diagnostic:?}");
        assert!(diagnostic.message.contains("payload"), "{diagnostic:?}");
        assert!(!diagnostic.message.contains("caller") && !diagnostic.message.contains("wrong"));
        assert_eq!(diagnostic.function, "caller");
        assert_eq!(
            diagnostic.filename.as_deref(),
            Some(b"caller.vibe".as_slice())
        );
        assert!(diagnostic.code_frame.contains("invoke(wrong)"));
        let offset = caller.program.functions[1].locations[0];
        let position = caller.program.source.position(offset);
        assert_eq!(diagnostic.offset, offset as usize);
        assert_eq!(diagnostic.position.line, position.line);
        assert_eq!(diagnostic.position.column, position.column);
        drop(report);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn cross_source_host_messages_release_callback_owners_without_invoking_them() {
    use crate::checking::{arguments::Failure, calls::Target};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let weak = Arc::downgrade(&calls);
    let callback = calls.clone();
    let mut engine = Engine::new();
    engine.register("foreign_host", move |_, _| {
        callback.fetch_add(1, Ordering::Relaxed);
        Ok(crate::Value::nil())
    });
    let callee = engine.compile("def callee; foreign_host(); end").unwrap();
    let caller = named("def caller; local_host(); end", Some(b"caller.vibe"));
    let mut ctx = CallContext::new(CallOptions::default());
    let report = call_report(
        &mut ctx,
        &caller,
        &callee.inner.code,
        |source| Target::Host(source.callable(0)),
        Failure::HostArity,
    )
    .unwrap();
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].message.contains("foreign_host"));
    assert_eq!(
        report.diagnostics[0].filename.as_deref(),
        Some(b"caller.vibe".as_slice())
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    drop((callee, engine, calls));
    assert!(weak.upgrade().is_none());
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn cross_source_call_message_failures_release_all_metadata_handles() {
    use crate::checking::{arguments::Failure, calls::Target};
    let caller = named(
        "def caller(wrong); invoke(wrong); end",
        Some(b"caller.vibe"),
    );
    let callee = named("def callee(payload); payload; end", Some(b"library.vibe"));
    let work = |ctx: &mut CallContext| {
        call_report(
            ctx,
            &caller,
            &callee,
            |source| Target::Function(source.callable(1)),
            Failure::Missing(0),
        )
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let report = work(&mut ctx).unwrap();
    let stats = report.stats;
    drop(report);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in [0, 1, 4, 8, 12, 15, 16, 17] {
            let steps = if sample == 17 {
                stats.steps - 1
            } else {
                stats.steps * sample / 16
            };
            let memory = if sample == 17 {
                stats.peak_memory_bytes - 1
            } else {
                stats.peak_memory_bytes * sample as usize / 16
            };
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: (kind == ErrorKind::Steps).then_some(steps),
                    memory_bytes: (kind == ErrorKind::Memory).then_some(memory),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let result = work(&mut ctx);
            if sample == 16 {
                drop(result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
