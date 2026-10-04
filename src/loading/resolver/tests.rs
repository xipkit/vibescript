use super::super::test_support::{Directory, canonical};
use super::*;
use crate::{CallOptions, CancellationToken, Limits};
use std::fs;

fn resolve(
    resolver: &Resolver,
    ctx: &mut CallContext,
    name: &[u8],
    caller: Option<&Origin>,
) -> Result<Option<(Origin, files::Source)>> {
    let mut candidates = resolver.candidates(ctx, name, caller)?;
    while let Some(candidate) = candidates.next(ctx)? {
        if let Some(source) = resolver.read(ctx, &candidate)? {
            return Ok(Some((candidate.origin(), source)));
        }
    }
    Ok(None)
}

fn candidate(resolver: &Resolver, ctx: &mut CallContext, name: &[u8]) -> Candidate {
    resolver
        .candidates(ctx, name, None)
        .unwrap()
        .next(ctx)
        .unwrap()
        .unwrap()
}

#[test]
fn canonical_root_paths_resolve_nested_unicode_modules_with_reserved_storage() {
    let directory = Directory::new();
    directory.write("root space/pkg/café.vibe", b"unicode module");
    let root = directory.0.join("root space");
    let resolver = Resolver::new(std::slice::from_ref(&root), &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let module = candidate(&resolver, &mut ctx, "pkg/café".as_bytes());
    assert_eq!(
        module.path.strip_prefix(canonical(&root)).unwrap(),
        Path::new("pkg").join("café.vibe")
    );
    let source = resolver.read(&mut ctx, &module).unwrap().unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"unicode module");
    drop(source);
    drop(module);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn search_preserves_root_precedence_and_falls_through_absent_or_misspelled_files() {
    let directory = Directory::new();
    directory.write("first/choice.vibe", b"first");
    directory.write("second/choice.vibe", b"second");
    directory.write("first/Tool.vibe", b"wrong case");
    directory.write("second/tool.vibe", b"exact case");
    directory.write("second/only.vibe", b"second only");
    let roots = [directory.0.join("first"), directory.0.join("second")];
    let resolver = Resolver::new(&roots, &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    for (name, expected_root, expected) in [
        ("choice", &roots[0], b"first".as_slice()),
        ("tool", &roots[1], b"exact case".as_slice()),
        ("only", &roots[1], b"second only".as_slice()),
    ] {
        let (origin, source) = resolve(&resolver, &mut ctx, name.as_bytes(), None)
            .unwrap()
            .unwrap();
        assert_eq!(origin.root.path(), canonical(expected_root));
        assert_eq!(source.contents.as_bytes().unwrap(), expected);
        assert_eq!(source.stamp.size, expected.len() as u64);
        drop(source);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    assert!(
        resolve(&resolver, &mut ctx, b"absent", None)
            .unwrap()
            .is_none()
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn an_existing_invalid_source_does_not_fall_through_to_another_root() {
    let directory = Directory::new();
    fs::create_dir_all(directory.0.join("first/tool.vibe")).unwrap();
    directory.write("second/tool.vibe", b"second");
    let resolver = Resolver::new(
        &[directory.0.join("first"), directory.0.join("second")],
        &[],
        &[],
        100,
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let error = resolve(&resolver, &mut ctx, b"tool", None).unwrap_err();
    assert!(error.message.contains("not a regular file"), "{error}");
    assert!(!ctx.exhausted());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn explicit_relative_requests_keep_their_origin_and_literal_filename_components() {
    let directory = Directory::new();
    directory.write("origin/pkg space/inner/start.vibe", b"start");
    directory.write("origin/pkg space/tool .vibe", b"literal space");
    directory.write("origin/pkg space/inner/name.vibe.vibe", b"double extension");
    directory.write("other/pkg space/tool .vibe", b"wrong origin");
    let producer = Resolver::new(&[directory.0.join("origin")], &[], &[], 100).unwrap();
    let receiver = Resolver::new(&[directory.0.join("other")], &[], &[], 100).unwrap();
    let rootless = Resolver::new(&[], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let origin = candidate(&producer, &mut ctx, b"pkg space/inner/start").origin();
    for resolver in [&producer, &receiver, &rootless] {
        for (request, expected_relative, expected) in [
            (
                "../tool /.",
                "pkg space/tool .vibe",
                b"literal space".as_slice(),
            ),
            (
                ".\\name.vibe.vibe",
                "pkg space/inner/name.vibe.vibe",
                b"double extension".as_slice(),
            ),
        ] {
            let (loaded, source) = resolve(resolver, &mut ctx, request.as_bytes(), Some(&origin))
                .unwrap()
                .unwrap();
            assert_eq!(loaded.root, origin.root);
            assert_eq!(loaded.relative.as_ref(), expected_relative.as_bytes());
            assert_eq!(source.contents.as_bytes().unwrap(), expected);
        }
        assert!(
            resolve(resolver, &mut ctx, b"./absent", Some(&origin))
                .unwrap()
                .is_none()
        );
        let error = resolve(resolver, &mut ctx, b"../../../outside", Some(&origin)).unwrap_err();
        assert!(error.message.contains("escapes module root"), "{error}");
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    let error = resolve(&rootless, &mut ctx, b"ordinary", Some(&origin)).unwrap_err();
    assert!(
        error.message.contains("module paths not configured"),
        "{error}"
    );
}

#[test]
fn receiver_policy_is_applied_to_the_normalized_origin_relative_name_before_io() {
    let directory = Directory::new();
    directory.write("root/pkg/start.vibe", b"start");
    directory.write("root/shared.vibe", b"shared");
    let producer = Resolver::new(&[directory.0.join("root")], &[], &[], 100).unwrap();
    let allowed = Resolver::new(&[], &["shared".into()], &[], 100).unwrap();
    let denied = Resolver::new(&[], &["*".into()], &["shared".into()], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let origin = candidate(&producer, &mut ctx, b"pkg/start").origin();
    let (_, source) = resolve(&allowed, &mut ctx, b"../shared", Some(&origin))
        .unwrap()
        .unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"shared");
    drop(source);
    #[cfg(not(windows))]
    fs::rename(directory.0.join("root"), directory.0.join("removed-root")).unwrap();
    #[cfg(windows)]
    fs::remove_file(directory.0.join("root/shared.vibe")).unwrap();
    let error = denied
        .candidates(&mut ctx, b"../shared", Some(&origin))
        .err()
        .unwrap();
    assert!(error.message.contains("denied by policy"), "{error}");
    let error = allowed
        .candidates(&mut ctx, b"./another", Some(&origin))
        .err()
        .unwrap();
    assert!(error.message.contains("not allowed by policy"), "{error}");
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn name_and_caller_errors_precede_missing_search_roots() {
    let resolver = Resolver::new(&[], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    for (name, message) in [
        (b"".as_slice(), "module name must be non-empty"),
        (b"/absolute".as_slice(), "module name must be relative"),
        (b"./relative".as_slice(), "requires a module caller"),
        (b"ordinary".as_slice(), "module paths not configured"),
    ] {
        let error = resolver.candidates(&mut ctx, name, None).err().unwrap();
        assert!(error.message.contains(message), "{name:?}: {error}");
        assert!(!ctx.exhausted());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn configuration_checks_roots_before_policy_and_preserves_literal_whitespace() {
    let directory = Directory::new();
    directory.write("literal root /tool.vibe", b"literal");
    let file = directory.write("file", b"file");
    let invalid_policy = ["[".into()];
    for path in [
        PathBuf::new(),
        PathBuf::from(" \t"),
        directory.0.join("missing"),
        file,
    ] {
        let error = Resolver::new(&[path], &invalid_policy, &[], 100)
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert!(!error.message.contains("pattern"), "{error}");
    }
    let error = Resolver::new(
        std::slice::from_ref(&directory.0),
        &invalid_policy,
        &[],
        100,
    )
    .err()
    .unwrap();
    assert!(error.message.contains("pattern"), "{error}");
    let resolver = Resolver::new(&[directory.0.join("literal root ")], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let (_, source) = resolve(&resolver, &mut ctx, b"tool", None)
        .unwrap()
        .unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"literal");
}

#[test]
fn candidate_creation_does_not_access_files_and_disappearing_sources_are_missing() {
    let directory = Directory::new();
    let path = directory.write("root/tool.vibe", b"tool");
    let root = directory.0.join("root");
    let resolver = Resolver::new(std::slice::from_ref(&root), &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let first = candidate(&resolver, &mut ctx, b"tool");
    fs::remove_file(path).unwrap();
    assert!(resolver.read(&mut ctx, &first).unwrap().is_none());
    // Windows keeps the configured root open without delete sharing.
    #[cfg(not(windows))]
    fs::rename(&root, directory.0.join("renamed")).unwrap();
    let later = candidate(&resolver, &mut ctx, b"dir/../tool");
    assert_eq!(first.origin(), later.origin());
    assert_eq!(first.path, later.path);
    drop(first);
    drop(later);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn overlapping_roots_retain_distinct_relative_authority() {
    let directory = Directory::new();
    directory.write("root/nested/tool.vibe", b"tool");
    let broad = Resolver::new(&[directory.0.join("root")], &[], &[], 100).unwrap();
    let narrow = Resolver::new(&[directory.0.join("root/nested")], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let first = candidate(&broad, &mut ctx, b"nested/tool");
    let second = candidate(&narrow, &mut ctx, b"tool");
    assert_eq!(first.path, second.path);
    assert_ne!(first.origin(), second.origin());
    let error = resolve(&narrow, &mut ctx, b"../outside", Some(&second.origin())).unwrap_err();
    assert!(error.message.contains("escapes module root"), "{error}");
    drop(first);
    drop(second);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn resolution_releases_allocations_on_source_limit_budget_and_cancellation_errors() {
    let directory = Directory::new();
    directory.write("root/large.vibe", &vec![b'x'; 65537]);
    let root = directory.0.join("root");
    let resolver = Resolver::new(std::slice::from_ref(&root), &[], &[], 65536).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let error = resolve(&resolver, &mut ctx, b"large", None).unwrap_err();
    assert!(
        error.message.contains("source exceeds maximum size"),
        "{error}"
    );
    assert!(!ctx.exhausted());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let resolver = Resolver::new(&[root], &[], &[], 65537).unwrap();
    for (limits, kind) in [
        (
            Limits {
                steps: Some(0),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
        (
            Limits {
                steps: None,
                memory_bytes: Some(32768),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits,
            ..CallOptions::default()
        });
        assert_eq!(
            resolve(&resolver, &mut ctx, b"large", None)
                .unwrap_err()
                .kind,
            kind
        );
        assert!(ctx.exhausted());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    let cancellation = CancellationToken::new();
    let mut ctx = CallContext::new(CallOptions {
        cancellation: cancellation.clone(),
        ..CallOptions::default()
    });
    let mut candidates = resolver.candidates(&mut ctx, b"large", None).unwrap();
    cancellation.cancel();
    assert_eq!(
        candidates.next(&mut ctx).err().unwrap().kind,
        ErrorKind::Cancelled
    );
    drop(candidates);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn configured_symlink_roots_are_canonicalized_once() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("first/tool.vibe", b"first");
    directory.write("second/tool.vibe", b"second");
    let link = directory.0.join("root");
    symlink("first", &link).unwrap();
    let resolver = Resolver::new(std::slice::from_ref(&link), &[], &[], 100).unwrap();
    fs::remove_file(&link).unwrap();
    symlink("second", &link).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let (origin, source) = resolve(&resolver, &mut ctx, b"tool", None)
        .unwrap()
        .unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"first");
    assert_eq!(
        origin.root.path(),
        fs::canonicalize(directory.0.join("first")).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn search_rejects_escaping_and_broken_symlinks_before_later_roots() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("first/start.vibe", b"start");
    directory.write("second/escape.vibe", b"later");
    directory.write("second/broken.vibe", b"later");
    directory.write("outside/secret.vibe", b"secret");
    let first = directory.0.join("first");
    symlink("../outside/secret.vibe", first.join("escape.vibe")).unwrap();
    symlink("absent", first.join("broken.vibe")).unwrap();
    let resolver = Resolver::new(&[first, directory.0.join("second")], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let origin = candidate(&resolver, &mut ctx, b"start").origin();
    for name in [b"escape".as_slice(), b"broken"] {
        let error = resolve(&resolver, &mut ctx, name, None).unwrap_err();
        assert!(error.message.contains("escapes module root"), "{error}");
    }
    assert!(
        resolve(&resolver, &mut ctx, b"./broken", Some(&origin))
            .unwrap()
            .is_none()
    );
    let error = resolve(&resolver, &mut ctx, b"./escape", Some(&origin)).unwrap_err();
    assert!(error.message.contains("escapes module root"), "{error}");
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn origins_do_not_conflate_delimiters_in_roots_and_names() {
    let directory = Directory::new();
    directory.write("a::b/c.vibe", b"first");
    directory.write("a/b::c.vibe", b"second");
    let resolver = Resolver::new(
        &[directory.0.join("a::b"), directory.0.join("a")],
        &[],
        &[],
        100,
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let (first, first_source) = resolve(&resolver, &mut ctx, b"c", None).unwrap().unwrap();
    let (second, second_source) = resolve(&resolver, &mut ctx, b"b::c", None)
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(first_source.contents.as_bytes().unwrap(), b"first");
    assert_eq!(second_source.contents.as_bytes().unwrap(), b"second");
}

#[test]
fn cached_source_validation_uses_spelling_mtime_and_size_without_reading_contents() {
    let directory = Directory::new();
    let path = directory.write("tool.vibe", b"abc");
    let resolver = Resolver::new(std::slice::from_ref(&directory.0), &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let candidate = candidate(&resolver, &mut ctx, b"tool");
    let source = resolver.read(&mut ctx, &candidate).unwrap().unwrap();
    assert!(resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(
        fs::FileTimes::new()
            .set_modified(source.stamp.modified + std::time::Duration::from_secs(5)),
    )
    .unwrap();
    assert!(!resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    directory.write("tool.vibe", b"xyz");
    file.set_times(fs::FileTimes::new().set_modified(source.stamp.modified))
        .unwrap();
    assert!(resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    directory.write("tool.vibe", b"longer");
    file.set_times(fs::FileTimes::new().set_modified(source.stamp.modified))
        .unwrap();
    assert!(!resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    drop(file);
    fs::remove_file(&path).unwrap();
    assert!(!resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    fs::create_dir(&path).unwrap();
    assert!(!resolver.valid(&mut ctx, &candidate, source.stamp).unwrap());
    assert!(!ctx.exhausted());
    drop(source);
    drop(candidate);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn cached_source_validation_propagates_cancellation_and_budget_failures() {
    let directory = Directory::new();
    directory.write("tool.vibe", b"abc");
    let resolver = Resolver::new(std::slice::from_ref(&directory.0), &[], &[], 100).unwrap();
    let mut setup = CallContext::new(CallOptions::default());
    let candidate = candidate(&resolver, &mut setup, b"tool");
    let stamp = resolver
        .read(&mut setup, &candidate)
        .unwrap()
        .unwrap()
        .stamp;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    for (options, kind) in [
        (
            CallOptions {
                cancellation,
                ..CallOptions::default()
            },
            ErrorKind::Cancelled,
        ),
        (
            CallOptions {
                limits: Limits {
                    steps: Some(0),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Steps,
        ),
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(1),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        let mut ctx = CallContext::new(options);
        assert_eq!(
            resolver
                .valid(&mut ctx, &candidate, stamp)
                .unwrap_err()
                .kind,
            kind
        );
        assert!(ctx.exhausted());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[cfg(unix)]
#[test]
fn cached_source_validation_rejects_symlinks_retargeted_outside_the_root() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("root/inside.vibe", b"abc");
    let outside = directory.write("outside.vibe", b"xyz");
    let link = directory.0.join("root/tool.vibe");
    symlink("inside.vibe", &link).unwrap();
    let resolver = Resolver::new(&[directory.0.join("root")], &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let candidate = candidate(&resolver, &mut ctx, b"tool");
    let stamp = resolver.read(&mut ctx, &candidate).unwrap().unwrap().stamp;
    assert!(resolver.valid(&mut ctx, &candidate, stamp).unwrap());
    fs::OpenOptions::new()
        .write(true)
        .open(outside)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(stamp.modified))
        .unwrap();
    fs::remove_file(&link).unwrap();
    symlink("../outside.vibe", &link).unwrap();
    assert!(!resolver.valid(&mut ctx, &candidate, stamp).unwrap());
    assert!(!ctx.exhausted());
}

#[cfg(unix)]
#[test]
fn renamed_roots_and_retained_origins_keep_the_original_directory_authority() {
    let directory = Directory::new();
    directory.write("root/pkg/start.vibe", b"original");
    directory.write("root/shared.vibe", b"original shared");
    let path = directory.0.join("root");
    let producer = Resolver::new(std::slice::from_ref(&path), &[], &[], 100).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let first = candidate(&producer, &mut ctx, b"pkg/start");
    let origin = first.origin();
    let stamp = producer.read(&mut ctx, &first).unwrap().unwrap().stamp;
    fs::rename(&path, directory.0.join("moved")).unwrap();
    directory.write("root/pkg/start.vibe", b"replacement");
    directory.write("root/shared.vibe", b"replacement shared");
    assert_eq!(
        producer
            .read(&mut ctx, &first)
            .unwrap()
            .unwrap()
            .contents
            .as_bytes()
            .unwrap(),
        b"original"
    );
    assert!(producer.valid(&mut ctx, &first, stamp).unwrap());
    let replacement = Resolver::new(&[path], &[], &[], 100).unwrap();
    let second = candidate(&replacement, &mut ctx, b"pkg/start");
    assert_eq!(first.path, second.path);
    assert_ne!(origin, second.origin());
    drop(first);
    drop(second);
    drop(producer);
    let rootless = Resolver::new(&[], &[], &[], 100).unwrap();
    let (_, source) = resolve(&rootless, &mut ctx, b"../shared", Some(&origin))
        .unwrap()
        .unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"original shared");
    drop(source);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn absolute_in_root_links_preserve_lexical_policy_and_relative_callers() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("Source/ExactFile.vibe", b"source");
    directory.write("Source/Helper.vibe", b"wrong");
    directory.write("Linked/Helper.vibe", b"correct");
    symlink(
        directory.0.join("Source/ExactFile.vibe"),
        directory.0.join("Linked/ExactLink.vibe"),
    )
    .unwrap();
    let resolver = Resolver::new(
        std::slice::from_ref(&directory.0),
        &[],
        &["Source/ExactFile".into()],
        100,
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let (origin, source) = resolve(&resolver, &mut ctx, b"Linked/ExactLink", None)
        .unwrap()
        .unwrap();
    assert_eq!(source.contents.as_bytes().unwrap(), b"source");
    assert_eq!(origin.relative.as_ref(), b"Linked/ExactLink.vibe");
    let (_, helper) = resolve(&resolver, &mut ctx, b"./Helper", Some(&origin))
        .unwrap()
        .unwrap();
    assert_eq!(helper.contents.as_bytes().unwrap(), b"correct");
    drop(source);
    drop(helper);
    assert!(
        resolve(&resolver, &mut ctx, b"Source/ExactFile", None)
            .unwrap_err()
            .message
            .contains("denied by policy")
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
