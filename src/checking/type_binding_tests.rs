use super::{
    calls::{self, Host, World},
    facts::{Atom, Fact, Facts, Node},
    relation::Relation,
    type_bindings::{Binding, Bindings, Resolution, Scope},
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Signature,
    SignatureParam, Value, budget::Buffer, bytecode, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = "enum Status; Draft; Sent; end; enum Review; Draft; end; class Widget; end;";

fn known(bindings: &Bindings, ctx: &mut CallContext, scopes: &[Scope], name: &str) -> Fact {
    let Resolution::Known(fact) = bindings.resolve(ctx, scopes, name, false).unwrap() else {
        panic!("{name}")
    };
    fact
}

fn nominal(fact: Fact, enumeration: bool) -> Binding {
    Binding::Type { fact, enumeration }
}

#[test]
fn optional_bindings_and_open_scopes_keep_unresolved_alternatives() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let first = facts.nominal(&mut ctx, 0, 0, b"State", None).unwrap();
    let second = facts.nominal(&mut ctx, 0, 1, b"State", None).unwrap();
    let mut bindings = Bindings::new();
    let local = bindings.scope(&mut ctx).unwrap();
    let outer = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, outer, b"State", nominal(first, false))
        .unwrap();
    bindings
        .optional(&mut ctx, local, b"State", nominal(second, false))
        .unwrap();
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[local, outer], "State", false)
            .unwrap(),
        Resolution::Dynamic
    );
    bindings
        .optional(&mut ctx, local, b"State", Binding::Other)
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[local, outer], "State"), first);
    bindings.open(&mut ctx, local).unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[local, outer], "State"), first);
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[local, outer], "Unknown", false)
            .unwrap(),
        Resolution::Dynamic
    );
    bindings
        .insert(&mut ctx, local, b"State", nominal(second, false))
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[local, outer], "State"), second);
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[local], "state", false)
            .unwrap(),
        Resolution::Dynamic
    );
    let root = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, root, b"api", Binding::Exports(local))
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[root], "api.State"), second);
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[root], "api.state", false)
            .unwrap(),
        Resolution::Dynamic
    );
    let copy = bindings.snapshot(&mut ctx).unwrap();
    assert_eq!(
        copy.resolve(&mut ctx, &[root], "api.Missing", false)
            .unwrap(),
        Resolution::Dynamic
    );
    drop((copy, bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn optional_type_candidates_cover_all_concrete_presence_combinations() {
    let mut checked = 0;
    for choices in 0..27 {
        for optional in 0..8 {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let first = facts.nominal(&mut ctx, 0, 0, b"First", None).unwrap();
            let second = facts.nominal(&mut ctx, 0, 1, b"Second", None).unwrap();
            let values = [Binding::Other, nominal(first, true), nominal(second, true)];
            let names = [b"State".as_slice(), b"STATE", b"sTate"];
            let mut abstract_bindings = Bindings::new();
            let local = abstract_bindings.scope(&mut ctx).unwrap();
            let outer = abstract_bindings.scope(&mut ctx).unwrap();
            abstract_bindings
                .insert(&mut ctx, outer, b"State", values[1])
                .unwrap();
            for (index, name) in names.iter().enumerate() {
                let value = values[(choices / 3usize.pow(index as u32)) % 3];
                if optional & (1 << index) != 0 {
                    abstract_bindings
                        .optional(&mut ctx, local, name, value)
                        .unwrap();
                } else {
                    abstract_bindings
                        .insert(&mut ctx, local, name, value)
                        .unwrap();
                }
            }
            let root = abstract_bindings.scope(&mut ctx).unwrap();
            abstract_bindings
                .insert(&mut ctx, root, b"api", Binding::Exports(local))
                .unwrap();
            for query in ["State", "STATE", "state", "api.State", "api.state"] {
                let abstract_result = abstract_bindings
                    .resolve(&mut ctx, &[root, local, outer], query, false)
                    .unwrap();
                let mut concrete_results = Vec::new();
                for present in 0..8 {
                    if present & !optional != !optional & 7 {
                        continue;
                    }
                    let mut concrete = Bindings::new();
                    let a = concrete.scope(&mut ctx).unwrap();
                    let b = concrete.scope(&mut ctx).unwrap();
                    concrete.insert(&mut ctx, b, b"State", values[1]).unwrap();
                    for (index, name) in names.iter().enumerate() {
                        if present & (1 << index) != 0 {
                            concrete
                                .insert(
                                    &mut ctx,
                                    a,
                                    name,
                                    values[(choices / 3usize.pow(index as u32)) % 3],
                                )
                                .unwrap();
                        }
                    }
                    let r = concrete.scope(&mut ctx).unwrap();
                    concrete
                        .insert(&mut ctx, r, b"api", Binding::Exports(a))
                        .unwrap();
                    concrete_results.push(
                        concrete
                            .resolve(&mut ctx, &[r, a, b], query, false)
                            .unwrap(),
                    );
                    checked += 1;
                }
                assert!(!concrete_results.is_empty());
                if abstract_result != Resolution::Dynamic {
                    assert!(
                        concrete_results
                            .iter()
                            .all(|result| *result == abstract_result),
                        "choices={choices},optional={optional},query={query},abstract={abstract_result:?},concrete={concrete_results:?}"
                    );
                }
                if concrete_results
                    .iter()
                    .all(|result| *result == Resolution::Known(first))
                {
                    assert_eq!(
                        abstract_result,
                        Resolution::Known(first),
                        "choices={choices},optional={optional},query={query}"
                    );
                }
            }
            drop((abstract_bindings, facts));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
    assert_eq!(checked, 3645);
}

#[test]
fn source_types_preserve_identity_enum_symbols_and_unexecuted_bodies() {
    let program = bytecode::compile(
        &format!("{SOURCE} module SideEffect; raise(\"must not run\"); end"),
        Vec::new(),
        &(),
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut bindings = Bindings::new();
    let first = bindings.source(&mut ctx, &mut facts, &program, 1).unwrap();
    let same = bindings.source(&mut ctx, &mut facts, &program, 1).unwrap();
    let other = bindings.source(&mut ctx, &mut facts, &program, 2).unwrap();
    let status = known(&bindings, &mut ctx, &[first], "Status");
    assert_eq!(status, known(&bindings, &mut ctx, &[same], "status"));
    assert_eq!(status, known(&bindings, &mut ctx, &[other], "Status"));
    let draft = facts.symbol(&mut ctx, b"draft").unwrap();
    let missing = facts.symbol(&mut ctx, b"missing").unwrap();
    assert_eq!(
        facts.relation(&mut ctx, draft, status).unwrap(),
        Relation::Accepted
    );
    assert_eq!(
        facts.relation(&mut ctx, missing, status).unwrap(),
        Relation::Rejected
    );
    let widget = known(&bindings, &mut ctx, &[first], "Widget");
    assert!(matches!(
        facts.node(widget),
        Node::Nominal { symbols: None, .. }
    ));
    assert_ne!(widget, known(&bindings, &mut ctx, &[other], "Widget"));
    known(&bindings, &mut ctx, &[first], "SideEffect");
    drop((bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn source_overlays_distinguish_replaced_declarations_from_lexical_non_types() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut bindings = Bindings::new();
    let original = bindings.source(&mut ctx, &mut facts, &program, 0).unwrap();
    let status = known(&bindings, &mut ctx, &[original], "Status");
    let replacement = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, replacement, b"Status", Binding::Other)
        .unwrap();
    assert_eq!(
        known(&bindings, &mut ctx, &[replacement, original], "Status"),
        status
    );
    let mut copy = bindings.snapshot(&mut ctx).unwrap();
    copy.overlay(&mut ctx, original, &[replacement]).unwrap();
    assert_eq!(
        copy.resolve(&mut ctx, &[replacement, original], "Status", false)
            .unwrap(),
        Resolution::Missing
    );
    assert_eq!(known(&bindings, &mut ctx, &[original], "Status"), status);
    for same in [false, true] {
        let mut copy = bindings.snapshot(&mut ctx).unwrap();
        copy.optional(
            &mut ctx,
            replacement,
            b"Status",
            if same {
                nominal(status, true)
            } else {
                Binding::Other
            },
        )
        .unwrap();
        copy.overlay(&mut ctx, original, &[replacement]).unwrap();
        assert_eq!(
            copy.resolve(&mut ctx, &[original], "Status", false)
                .unwrap(),
            if same {
                Resolution::Known(status)
            } else {
                Resolution::Dynamic
            }
        );
    }
    let higher = bindings.scope(&mut ctx).unwrap();
    bindings
        .optional(&mut ctx, higher, b"Status", nominal(status, true))
        .unwrap();
    let mut layered = bindings.snapshot(&mut ctx).unwrap();
    layered
        .overlay(&mut ctx, original, &[higher, replacement])
        .unwrap();
    assert_eq!(
        layered
            .resolve(&mut ctx, &[original], "Status", false)
            .unwrap(),
        Resolution::Dynamic
    );
    drop(layered);
    let unknown = bindings.scope(&mut ctx).unwrap();
    bindings.open(&mut ctx, unknown).unwrap();
    bindings.overlay(&mut ctx, original, &[unknown]).unwrap();
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[original], "Status", false)
            .unwrap(),
        Resolution::Dynamic
    );
    drop((copy, bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn exact_names_across_scopes_precede_folded_names_and_non_types_fall_through() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut bindings = Bindings::new();
    let source = bindings.source(&mut ctx, &mut facts, &program, 0).unwrap();
    let status = known(&bindings, &mut ctx, &[source], "Status");
    let review = known(&bindings, &mut ctx, &[source], "Review");
    let local = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, local, b"STATUS", nominal(review, true))
        .unwrap();
    assert_eq!(
        known(&bindings, &mut ctx, &[local, source], "Status"),
        status
    );
    assert_eq!(
        known(&bindings, &mut ctx, &[local, source], "STATUS"),
        review
    );
    assert_eq!(
        known(&bindings, &mut ctx, &[local, source], "status"),
        review
    );
    bindings
        .insert(&mut ctx, local, b"Status", Binding::Other)
        .unwrap();
    assert_eq!(
        known(&bindings, &mut ctx, &[local, source], "Status"),
        status
    );
    bindings
        .insert(&mut ctx, local, b"Status", Binding::Unknown)
        .unwrap();
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[local, source], "Status", false)
            .unwrap(),
        Resolution::Dynamic
    );
    bindings
        .insert(&mut ctx, local, b"Status", nominal(status, true))
        .unwrap();
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[local, source], "status", false)
            .unwrap(),
        Resolution::Ambiguous
    );
    bindings
        .insert(&mut ctx, local, b"STATUS", nominal(status, true))
        .unwrap();
    assert_eq!(
        known(&bindings, &mut ctx, &[local, source], "status"),
        status
    );
    for alias in ["ÄType", "ΣType"] {
        bindings
            .insert(&mut ctx, local, alias.as_bytes(), nominal(status, true))
            .unwrap();
    }
    assert_eq!(known(&bindings, &mut ctx, &[local], "ätype"), status);
    assert_eq!(known(&bindings, &mut ctx, &[local], "σtype"), status);
    drop((bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn qualified_exports_keep_exact_roots_folded_members_and_enum_only_rules() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut bindings = Bindings::new();
    let source = bindings.source(&mut ctx, &mut facts, &program, 0).unwrap();
    let status = known(&bindings, &mut ctx, &[source], "Status");
    let widget = known(&bindings, &mut ctx, &[source], "Widget");
    let root = bindings.scope(&mut ctx).unwrap();
    let exports = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, root, b"api", Binding::Exports(exports))
        .unwrap();
    bindings
        .insert(&mut ctx, exports, b"State", nominal(status, true))
        .unwrap();
    bindings
        .insert(&mut ctx, exports, b"STATE", nominal(widget, false))
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[root], "api.State"), status);
    assert_eq!(known(&bindings, &mut ctx, &[root], "api.STATE"), widget);
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[root], "api.state", false)
            .unwrap(),
        Resolution::Ambiguous
    );
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[root], "API.State", false)
            .unwrap(),
        Resolution::Missing
    );
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[root], "api.STATE", true)
            .unwrap(),
        Resolution::Known(status)
    );
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[source], "Widget", true)
            .unwrap(),
        Resolution::Known(widget)
    );
    bindings
        .insert(&mut ctx, exports, b"State", Binding::Other)
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[root], "api.State"), widget);
    bindings
        .insert(&mut ctx, exports, b"State", Binding::Unknown)
        .unwrap();
    assert_eq!(
        bindings
            .resolve(&mut ctx, &[root], "api.State", false)
            .unwrap(),
        Resolution::Dynamic
    );
    drop((bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn binding_snapshots_isolate_replacements_and_handle_cyclic_export_graphs() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let status = facts
        .nominal(&mut ctx, 0, 0, b"Status", Some(&[b"draft"]))
        .unwrap();
    let mut bindings = Bindings::new();
    let root = bindings.scope(&mut ctx).unwrap();
    bindings
        .insert(&mut ctx, root, b"Status", nominal(status, true))
        .unwrap();
    let mut previous = root;
    for _ in 0..512 {
        let scope = bindings.scope(&mut ctx).unwrap();
        bindings
            .insert(&mut ctx, previous, b"next", Binding::Exports(scope))
            .unwrap();
        previous = scope;
    }
    bindings
        .insert(&mut ctx, previous, b"next", Binding::Exports(root))
        .unwrap();
    let mut copy = bindings.snapshot(&mut ctx).unwrap();
    copy.insert(&mut ctx, root, b"Status", Binding::Unknown)
        .unwrap();
    assert_eq!(known(&bindings, &mut ctx, &[root], "Status"), status);
    assert_eq!(
        copy.resolve(&mut ctx, &[root], "Status", false).unwrap(),
        Resolution::Dynamic
    );
    assert_eq!(
        copy.resolve(&mut ctx, &[root], "next.next", false).unwrap(),
        Resolution::Missing
    );
    drop((bindings, copy, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn source_annotations_enable_nominal_script_argument_and_return_contracts() {
    for (body, expected, rejected) in [
        (
            "def echo(x:Status)->Status; x; end; def run; echo(:draft); end",
            Some("Status::Draft"),
            false,
        ),
        (
            "def echo(x:status)->STATUS; x; end; def run; echo(:sent); end",
            Some("Status::Sent"),
            false,
        ),
        (
            "def echo(x:Status); x; end; def run; echo(:missing); end",
            None,
            true,
        ),
        (
            "def echo(x:Widget); x; end; def run; echo(:draft); end",
            None,
            true,
        ),
        (
            "def echo(x:array<{state:Status}>); x; end; def run; echo([{state: :draft}]); end",
            Some("[{state: Status::Draft}]"),
            false,
        ),
    ] {
        let source = format!("{SOURCE}{body}");
        let script = Engine::new().compile(&source).unwrap();
        let program = &script.inner.code.program;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut bindings = Bindings::new();
        let scope = bindings.source(&mut ctx, &mut facts, program, 0).unwrap();
        let mut contracts = Buffer::empty();
        for ty in &program.types {
            let fact = facts
                .annotation(&mut ctx, ty, |ctx, name| {
                    Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
                })
                .unwrap();
            contracts.push(&mut ctx, fact).unwrap();
        }
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &contracts.data,
                hosts: &[],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert_eq!(
            !report.issues.data.is_empty(),
            rejected,
            "{source}: {report:?}"
        );
        let actual = script.call("run", &[], CallOptions::default());
        if let Some(expected) = expected {
            assert_eq!(actual.unwrap().value.to_string(), expected, "{source}");
            assert_ne!(report.returns, Atom::Never.fact());
        } else {
            assert_eq!(actual.unwrap_err().kind, ErrorKind::Type);
        }
        drop((report, contracts, bindings, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn resolved_host_contracts_validate_nominal_arguments_without_running_the_host() {
    for (ty, argument, expected) in [
        ("Status", ":draft", Some("Status::Draft")),
        ("Status", ":missing", None),
        ("Widget", "7", None),
        (
            "array<{state:Status}>",
            "[{state: :sent}]",
            Some("[{state: Status::Sent}]"),
        ),
        ("array<{state:Status}>", "[{state: :missing}]", None),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let method = HostMethod::new("echo", move |_, args, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(args[0].clone())
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "value".into(),
                ty: ty.into(),
                optional: false,
            }],
            result: ty.into(),
            accepts_block: false,
        })
        .unwrap();
        let mut engine = Engine::new();
        engine.register_method("echo", method.clone());
        let source = format!("{SOURCE} def run; echo({argument}); end");
        let script = engine.compile(&source).unwrap();
        let program = &script.inner.code.program;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut bindings = Bindings::new();
        let scope = bindings.source(&mut ctx, &mut facts, program, 0).unwrap();
        let value = method.value();
        let Kind::Host(method) = &value.0 else {
            panic!()
        };
        let host = Host::resolved(&mut ctx, &mut facts, method.signature(), |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })
        .unwrap();
        assert!(!host.unresolved);
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &[],
                hosts: &[host],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert_eq!(
            !report.issues.data.is_empty(),
            expected.is_none(),
            "{source}: {report:?}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let result = script.call("run", &[], CallOptions::default());
        if let Some(expected) = expected {
            assert_eq!(result.unwrap().value.to_string(), expected);
            assert_eq!(count.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Type);
            assert_eq!(count.load(Ordering::SeqCst), 0);
        }
        drop((report, bindings, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn missing_ambiguous_and_dynamic_host_type_bindings_remain_explicit() {
    let signature = crate::signature::Compiled::new(
        "echo",
        Signature {
            params: Vec::new(),
            result: "state".into(),
            accepts_block: false,
        },
    )
    .unwrap();
    for mode in 0..3 {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut bindings = Bindings::new();
        let scope = bindings.scope(&mut ctx).unwrap();
        if mode == 1 {
            for (index, name) in [b"State".as_slice(), b"STATE"].into_iter().enumerate() {
                let fact = facts.nominal(&mut ctx, 0, index, name, None).unwrap();
                bindings
                    .insert(&mut ctx, scope, name, nominal(fact, false))
                    .unwrap();
            }
        } else if mode == 2 {
            bindings
                .insert(&mut ctx, scope, b"state", Binding::Unknown)
                .unwrap();
        }
        let expected = [
            Resolution::Missing,
            Resolution::Ambiguous,
            Resolution::Dynamic,
        ][mode];
        assert_eq!(
            bindings
                .resolve(&mut ctx, &[scope], "state", false)
                .unwrap(),
            expected
        );
        let host = Host::resolved(&mut ctx, &mut facts, Some(&signature), |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })
        .unwrap();
        assert!(host.unresolved);
        let program = bytecode::compile("def run; echo(); end", vec!["echo".into()], &()).unwrap();
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program: &program,
                contracts: &[],
                hosts: &[host],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(!report.incomplete.data.is_empty());
        drop((report, bindings, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let signature = crate::signature::Compiled::new(
        "echo",
        Signature {
            params: Vec::new(),
            result: "array<api.Status?>".into(),
            accepts_block: false,
        },
    )
    .unwrap();
    let mut facts = Facts::new(ctx)?;
    let mut bindings = Bindings::new();
    for owner in 0..16 {
        let source = bindings.source(ctx, &mut facts, &program, owner)?;
        let root = bindings.scope(ctx)?;
        bindings.insert(ctx, root, b"api", Binding::Exports(source))?;
        assert!(matches!(
            bindings.resolve(ctx, &[root], "api.status", true)?,
            Resolution::Known(_)
        ));
        assert_eq!(
            bindings.resolve(ctx, &[root], "api.Widget", true)?,
            Resolution::Missing
        );
        let host = Host::resolved(ctx, &mut facts, Some(&signature), |ctx, name| {
            Ok(bindings.resolve(ctx, &[root], name, false)?.fact())
        })?;
        assert!(!host.unresolved);
        bindings.optional(ctx, root, b"Status", Binding::Other)?;
        bindings.overlay(ctx, source, &[root])?;
        let host = Host::resolved(ctx, &mut facts, Some(&signature), |ctx, name| {
            Ok(bindings.resolve(ctx, &[root], name, false)?.fact())
        })?;
        assert!(host.unresolved);
        bindings.open(ctx, root)?;
    }
    let _copy = bindings.snapshot(ctx)?;
    Ok(())
}

#[test]
fn type_binding_environments_obey_exact_quotas_and_reclaim_partial_snapshots() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, expected) in [
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
        assert_eq!(accounting(&mut ctx).err().map(|error| error.kind), expected);
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..64 {
        for memory in [false, true] {
            let limits = if memory {
                Limits {
                    memory_bytes: Some(stats.peak_memory_bytes * sample / 64),
                    ..Limits::default()
                }
            } else {
                Limits {
                    steps: Some(stats.steps * sample as u64 / 64),
                    ..Limits::default()
                }
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let kind = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            };
            assert_eq!(accounting(&mut ctx).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn cached_type_names_and_replacements_observe_latched_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut bindings = Bindings::new();
        let scope = bindings.scope(&mut ctx).unwrap();
        bindings
            .insert(&mut ctx, scope, b"State", Binding::Other)
            .unwrap();
        bindings
            .resolve(&mut ctx, &[scope], "State", false)
            .unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.options.cancellation.cancel();
        }
        let error = bindings
            .resolve(&mut ctx, &[scope], "State", false)
            .unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(
            bindings
                .insert(&mut ctx, scope, b"State", Binding::Unknown)
                .unwrap_err(),
            error
        );
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
        drop(bindings);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn named_binding_results_match_executed_host_signature_resolution() {
    let mut cases = 0;
    for scenario in 0..12 {
        for name in [
            "Status",
            "status",
            "State",
            "STATE",
            "state",
            "api.State",
            "api.STATE",
            "api.state",
            "API.State",
            "api.Widget",
            "Widget",
            "missing",
            "api.missing",
            "ätype",
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let observed = count.clone();
            let method = HostMethod::new("probe", move |_, _, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            })
            .with_signature(Signature {
                params: Vec::new(),
                result: format!("{name}?"),
                accepts_block: false,
            })
            .unwrap();
            let mut engine = Engine::new();
            engine.register_method("probe", method.clone());
            let script = engine
                .compile(&format!("{SOURCE} def run; probe(); end"))
                .unwrap();
            let program = &script.inner.code.program;
            let declaration =
                |name: &str| program.declarations[program.declaration_names[name]].clone();
            let globals = match scenario {
                0 => vec![],
                1 => vec![
                    ("State", declaration("Status")),
                    ("STATE", declaration("Status")),
                ],
                2 => vec![
                    ("State", declaration("Status")),
                    ("STATE", declaration("Review")),
                ],
                3 => vec![("Status", Value::int(7))],
                4 => vec![("State", Value::int(7)), ("STATE", declaration("Review"))],
                5 => vec![("Status", declaration("Review"))],
                6 => vec![
                    ("STATE", declaration("Status")),
                    ("State", declaration("Widget")),
                ],
                7 => vec![(
                    "api",
                    Value::object(vec![
                        (b"State".to_vec(), declaration("Status")),
                        (b"STATE".to_vec(), declaration("Review")),
                    ]),
                )],
                8 => vec![(
                    "api",
                    Value::object(vec![
                        (b"State".to_vec(), declaration("Widget")),
                        (b"STATE".to_vec(), declaration("Status")),
                    ]),
                )],
                9 => vec![(
                    "API",
                    Value::object(vec![(b"State".to_vec(), declaration("Status"))]),
                )],
                10 => vec![(
                    "api",
                    Value::hash(vec![(b"State".to_vec(), declaration("Status"))]),
                )],
                _ => vec![
                    (
                        "api",
                        Value::object(vec![
                            (b"State".to_vec(), Value::int(7)),
                            (b"STATE".to_vec(), declaration("Status")),
                        ]),
                    ),
                    ("ÄType", declaration("Status")),
                ],
            };
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let mut bindings = Bindings::new();
            let source = bindings.source(&mut ctx, &mut facts, program, 0).unwrap();
            let root = bindings.scope(&mut ctx).unwrap();
            fn value_binding(
                bindings: &Bindings,
                ctx: &mut CallContext,
                source: Scope,
                value: &Value,
            ) -> Binding {
                match &value.0 {
                    Kind::Enum(enumeration) => nominal(
                        known(bindings, ctx, &[source], &enumeration.definition.name),
                        true,
                    ),
                    Kind::Namespace(namespace) => nominal(
                        known(bindings, ctx, &[source], &namespace.definition.name),
                        false,
                    ),
                    _ => Binding::Other,
                }
            }
            for (name, value) in &globals {
                let value = if let Kind::Hash(hash) = &value.0 {
                    if hash.object {
                        let scope = bindings.scope(&mut ctx).unwrap();
                        for (key, value) in &hash.buffer.data {
                            let value = value_binding(&bindings, &mut ctx, source, value);
                            bindings
                                .insert(&mut ctx, scope, key.as_bytes().unwrap(), value)
                                .unwrap();
                        }
                        Binding::Exports(scope)
                    } else {
                        Binding::Other
                    }
                } else {
                    value_binding(&bindings, &mut ctx, source, value)
                };
                bindings
                    .insert(&mut ctx, root, name.as_bytes(), value)
                    .unwrap();
            }
            bindings.overlay(&mut ctx, source, &[root]).unwrap();
            let scopes = [root, source];
            let resolution = bindings.resolve(&mut ctx, &scopes, name, false).unwrap();
            let value = method.value();
            let Kind::Host(method) = &value.0 else {
                panic!()
            };
            let host = Host::resolved(&mut ctx, &mut facts, method.signature(), |ctx, name| {
                Ok(bindings.resolve(ctx, &scopes, name, false)?.fact())
            })
            .unwrap();
            let report = calls::analyze(
                &mut ctx,
                &mut facts,
                World {
                    loader: None,
                    inputs: &[],
                    source_owner: 0,
                    program,
                    contracts: &[],
                    hosts: &[host],
                    globals: &[],
                },
                program.names["run"],
                &[],
            )
            .unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            let actual = script.call(
                "run",
                &[],
                CallOptions {
                    globals: globals
                        .into_iter()
                        .map(|(name, value)| (name.to_owned(), value))
                        .collect(),
                    ..CallOptions::default()
                },
            );
            let label = format!("scenario={scenario}, name={name}, resolution={resolution:?}");
            match resolution {
                Resolution::Known(_) => {
                    assert!(
                        matches!(
                            actual.unwrap_or_else(|e| panic!("{label}: {e}")).value.0,
                            Kind::Nil
                        ),
                        "{label}"
                    );
                    assert!(report.incomplete.data.is_empty(), "{label}");
                }
                Resolution::Missing | Resolution::Ambiguous => {
                    let error = actual.unwrap_err();
                    assert_eq!(error.kind, ErrorKind::Type, "{label}: {error}");
                    let expected = if resolution == Resolution::Missing {
                        "unknown type"
                    } else {
                        "ambiguous "
                    };
                    assert!(error.message.contains(expected), "{label}: {error}");
                    assert!(!report.incomplete.data.is_empty(), "{label}");
                }
                Resolution::Dynamic | Resolution::Pending(_) => {
                    panic!("all fixture bindings are concrete: {label}")
                }
            }
            assert_eq!(count.load(Ordering::SeqCst), 1, "{label}");
            drop((report, bindings, facts));
            assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
            cases += 1;
        }
    }
    assert_eq!(cases, 168);
}
