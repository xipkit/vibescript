use super::super::{resolver::Resolver, test_support::Directory};
use super::*;
use crate::{CallOptions, CancellationToken, Limits, Script, Value};
use std::{
    fs,
    sync::{
        Barrier, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

fn load(
    cache: &Cache<Script>,
    resolver: &Resolver,
    ctx: &mut CallContext,
    name: &[u8],
) -> Result<Arc<Entry<Script>>> {
    let mut candidates = resolver.candidates(ctx, name, None)?;
    while let Some(candidate) = candidates.next(ctx)? {
        let (epoch, entry) =
            cache.lookup(ctx, &candidate.root, candidate.relative.as_bytes().unwrap())?;
        if let Some(entry) = entry {
            return Ok(entry);
        }
        let Some(source) = resolver.read(ctx, &candidate)? else {
            continue;
        };
        let code = crate::Engine::new()
            .compile(std::str::from_utf8(source.contents.as_bytes().unwrap()).unwrap())?;
        return cache.insert(ctx, &epoch, candidate.origin(), source.stamp, code);
    }
    Err(Error::new(ErrorKind::Name, "test module not found"))
}

fn value(entry: &Entry<Script>) -> i64 {
    entry
        .code
        .call("value", &[], CallOptions::default())
        .unwrap()
        .value
        .as_int()
        .unwrap()
}

fn fixture() -> (Directory, Resolver) {
    let directory = Directory::new();
    directory.write("one.vibe", b"def value; 1; end");
    directory.write("two.vibe", b"def value; 2; end");
    let resolver = Resolver::new(std::slice::from_ref(&directory.0), &[], &[], 1000).unwrap();
    (directory, resolver)
}

#[test]
fn compiled_versions_survive_file_changes_and_cache_clear() {
    let (directory, resolver) = fixture();
    let cache = Cache::new(10);
    let mut ctx = CallContext::new(CallOptions::default());
    let first = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    directory.write("one.vibe", b"def value; 7; end");
    let cached = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    assert!(Arc::ptr_eq(&first, &cached));
    assert_eq!(value(&cached), 1);
    fs::remove_file(directory.0.join("one.vibe")).unwrap();
    assert_eq!(
        value(&load(&cache, &resolver, &mut ctx, b"one").unwrap()),
        1
    );
    cache.clear();
    assert_eq!(value(&first), 1);
    assert!(load(&cache, &resolver, &mut ctx, b"one").is_err());
    directory.write("one.vibe", b"def value; 9; end");
    let current = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    assert_eq!(value(&current), 9);
    assert!(!Arc::ptr_eq(&first, &current));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn cache_keys_distinguish_replacement_roots_at_the_same_path() {
    let directory = Directory::new();
    directory.write("root/one.vibe", b"def value; 1; end");
    let path = directory.0.join("root");
    let original = Resolver::new(std::slice::from_ref(&path), &[], &[], 1000).unwrap();
    let cache = Cache::new(10);
    let mut ctx = CallContext::new(CallOptions::default());
    let first = load(&cache, &original, &mut ctx, b"one").unwrap();
    fs::rename(&path, directory.0.join("moved")).unwrap();
    directory.write("root/one.vibe", b"def value; 2; end");
    let replacement = Resolver::new(&[path], &[], &[], 1000).unwrap();
    let second = load(&cache, &replacement, &mut ctx, b"one").unwrap();
    assert_eq!(value(&first), 1);
    assert_eq!(value(&second), 2);
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(
        &load(&cache, &original, &mut ctx, b"one").unwrap(),
        &first
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn a_full_cache_rejects_new_modules_without_evicting_existing_code() {
    let (_directory, resolver) = fixture();
    let cache = Cache::new(1);
    let mut ctx = CallContext::new(CallOptions::default());
    let first = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    let error = load(&cache, &resolver, &mut ctx, b"two").err().unwrap();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(
        error
            .message
            .contains("module cache limit reached (1 modules)")
    );
    assert!(!ctx.exhausted());
    let existing = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    assert!(Arc::ptr_eq(&first, &existing));
    cache.invalidate(&mut ctx, &first).unwrap();
    assert_eq!(
        value(&load(&cache, &resolver, &mut ctx, b"two").unwrap()),
        2
    );
    assert_eq!(value(&first), 1);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn invalidating_an_old_observation_preserves_a_concurrent_replacement() {
    let (directory, resolver) = fixture();
    let cache = Cache::new(1);
    let mut ctx = CallContext::new(CallOptions::default());
    let first = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    cache.invalidate(&mut ctx, &first).unwrap();
    directory.write("one.vibe", b"def value; 8; end");
    let replacement = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    cache.invalidate(&mut ctx, &first).unwrap();
    let (_, present) = cache
        .lookup(&mut ctx, &first.origin.root, &first.origin.relative)
        .unwrap();
    assert!(Arc::ptr_eq(&present.unwrap(), &replacement));
    assert_eq!(value(&replacement), 8);
    assert!(load(&cache, &resolver, &mut ctx, b"two").is_err());
}

#[test]
fn a_compilation_started_before_clear_does_not_repopulate_the_cleared_cache() {
    let (directory, resolver) = fixture();
    let cache = Cache::new(1);
    let mut ctx = CallContext::new(CallOptions::default());
    let candidate = resolver
        .candidates(&mut ctx, b"one", None)
        .unwrap()
        .next(&mut ctx)
        .unwrap()
        .unwrap();
    let (before_clear, absent) = cache
        .lookup(
            &mut ctx,
            &candidate.root,
            candidate.relative.as_bytes().unwrap(),
        )
        .unwrap();
    assert!(absent.is_none());
    let source = resolver.read(&mut ctx, &candidate).unwrap().unwrap();
    let original = crate::Engine::new()
        .compile(std::str::from_utf8(source.contents.as_bytes().unwrap()).unwrap())
        .unwrap();
    cache.clear();
    directory.write("one.vibe", b"def value; 4; end");
    let current = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    let in_flight = cache
        .insert(
            &mut ctx,
            &before_clear,
            candidate.origin(),
            source.stamp,
            original,
        )
        .unwrap();
    assert_eq!(value(&in_flight), 1);
    assert_eq!(value(&current), 4);
    let cached = load(&cache, &resolver, &mut ctx, b"one").unwrap();
    assert!(Arc::ptr_eq(&current, &cached));
}

#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI has no threads")]
fn concurrent_compilations_publish_one_shared_entry() {
    let (_directory, resolver) = fixture();
    let cache = Cache::new(1);
    let ready = Barrier::new(8);
    let entries = std::thread::scope(|scope| {
        let jobs = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let mut ctx = CallContext::new(CallOptions::default());
                    let candidate = resolver
                        .candidates(&mut ctx, b"one", None)
                        .unwrap()
                        .next(&mut ctx)
                        .unwrap()
                        .unwrap();
                    let (epoch, absent) = cache
                        .lookup(
                            &mut ctx,
                            &candidate.root,
                            candidate.relative.as_bytes().unwrap(),
                        )
                        .unwrap();
                    assert!(absent.is_none());
                    let source = resolver.read(&mut ctx, &candidate).unwrap().unwrap();
                    let code = crate::Engine::new()
                        .compile(std::str::from_utf8(source.contents.as_bytes().unwrap()).unwrap())
                        .unwrap();
                    ready.wait();
                    cache
                        .insert(&mut ctx, &epoch, candidate.origin(), source.stamp, code)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    for entry in &entries {
        assert!(Arc::ptr_eq(&entries[0], entry));
        assert_eq!(value(entry), 1);
    }
}

struct RetiredHost {
    cache: Weak<Cache<Script>>,
    observed: Arc<AtomicUsize>,
}

impl Drop for RetiredHost {
    fn drop(&mut self) {
        if let Some(cache) = self.cache.upgrade() {
            let unlocked = cache.state.try_lock().is_ok();
            self.observed
                .store(if unlocked { 2 } else { 1 }, Ordering::SeqCst);
            if unlocked {
                cache.clear();
            }
        }
    }
}

fn retained_host(cache: &Arc<Cache<Script>>, observed: &Arc<AtomicUsize>) -> Script {
    let retired = RetiredHost {
        cache: Arc::downgrade(cache),
        observed: observed.clone(),
    };
    let mut engine = crate::Engine::new();
    engine.register("host", move |_, _| {
        let _ = &retired;
        Ok(Value::int(3))
    });
    engine.compile("def value; host(); end").unwrap()
}

#[test]
fn retiring_host_callbacks_runs_after_releasing_the_cache_lock() {
    let (_directory, resolver) = fixture();
    for operation in ["clear", "duplicate", "full"] {
        let cache = Arc::new(Cache::new(1));
        let observed = Arc::new(AtomicUsize::new(0));
        let mut ctx = CallContext::new(CallOptions::default());
        if operation != "clear" {
            load(&cache, &resolver, &mut ctx, b"one").unwrap();
        }
        let name = if operation == "full" {
            b"two".as_slice()
        } else {
            b"one"
        };
        let candidate = resolver
            .candidates(&mut ctx, name, None)
            .unwrap()
            .next(&mut ctx)
            .unwrap()
            .unwrap();
        let (epoch, _) = cache
            .lookup(
                &mut ctx,
                &candidate.root,
                candidate.relative.as_bytes().unwrap(),
            )
            .unwrap();
        let stamp = Stamp {
            modified: SystemTime::UNIX_EPOCH,
            size: 0,
        };
        let code = retained_host(&cache, &observed);
        let result = cache.insert(&mut ctx, &epoch, candidate.origin(), stamp, code);
        if operation == "full" {
            assert!(result.is_err());
        } else {
            drop(result.unwrap());
        }
        if operation == "clear" {
            assert_eq!(observed.load(Ordering::SeqCst), 0);
            cache.clear();
        }
        assert_eq!(observed.load(Ordering::SeqCst), 2, "{operation}");
    }
}

#[test]
fn cancellation_and_exhausted_work_do_not_modify_cached_entries() {
    let (_directory, resolver) = fixture();
    let cache = Cache::new(2);
    let mut setup = CallContext::new(CallOptions::default());
    let original = load(&cache, &resolver, &mut setup, b"one").unwrap();
    let (epoch, _) = cache
        .lookup(&mut setup, &original.origin.root, &original.origin.relative)
        .unwrap();
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
    ] {
        let mut ctx = CallContext::new(options);
        assert_eq!(
            cache
                .lookup(&mut ctx, &original.origin.root, &original.origin.relative)
                .err()
                .unwrap()
                .kind,
            kind
        );
        assert_eq!(
            cache.invalidate(&mut ctx, &original).unwrap_err().kind,
            kind
        );
        assert_eq!(
            cache
                .insert(
                    &mut ctx,
                    &epoch,
                    original.origin.clone(),
                    original.stamp,
                    original.code.clone()
                )
                .err()
                .unwrap()
                .kind,
            kind
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let (_, current) = cache
            .lookup(&mut setup, &original.origin.root, &original.origin.relative)
            .unwrap();
        assert!(Arc::ptr_eq(&original, &current.unwrap()));
    }
}
