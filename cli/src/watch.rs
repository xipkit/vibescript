//! `vibes run -watch`: re-run a script when it or its modules change.
//!
//! This is the reference's polling mode. Every interval, the stamps (size and
//! modification time) of the files seen so far are compared; every twenty
//! intervals, capped at five seconds, the module directories are walked again
//! to find added and deleted files. A change re-runs the script with a fresh
//! engine, so modules load again. Failures are reported without ending the
//! watch; an interrupt stops it.

use crate::{output::Sink, run};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// How often known files are compared.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(300);
/// The longest wait between full scans of the module directories.
const DEFAULT_FULL_SCAN_INTERVAL: Duration = Duration::from_secs(5);
/// The longest sleep between checks of the stop condition.
const STOP_POLL: Duration = Duration::from_millis(20);

/// A watched file's change signature; `None` when it could not be stat'ed.
type Stamp = Option<(SystemTime, u64)>;
type Snapshot = HashMap<PathBuf, Stamp>;

/// Runs the script, then re-runs it on every change until `stop` reports true.
pub fn watch(
    invocation: &run::Invocation,
    interval: Duration,
    stop: &dyn Fn() -> bool,
    out: &Sink,
    status: &Sink,
) -> Result<(), String> {
    let interval = if interval.is_zero() {
        DEFAULT_INTERVAL
    } else {
        interval
    };
    let full_scan = full_scan_interval(interval);
    let mut snapshot = snapshot(invocation);
    status.line(&format!(
        "watching {} file(s); press ctrl-c to stop",
        snapshot.len()
    ));
    run_watched(invocation, out, status);
    let mut next_tick = Instant::now() + interval;
    let mut next_scan = Instant::now() + full_scan;
    loop {
        let now = Instant::now();
        let wake = next_tick.min(next_scan);
        if now < wake {
            if stop() {
                break;
            }
            thread::sleep((wake - now).min(STOP_POLL));
            continue;
        }
        if stop() {
            break;
        }
        if now >= next_scan {
            next_scan = now + full_scan;
            next_tick = now + interval;
            rerun_if_changed(invocation, &mut snapshot, out, status);
        } else {
            next_tick = now + interval;
            if known_changed(&snapshot) {
                rerun_if_changed(invocation, &mut snapshot, out, status);
            }
        }
    }
    status.line("watch stopped");
    Ok(())
}

fn full_scan_interval(interval: Duration) -> Duration {
    interval
        .checked_mul(20)
        .unwrap_or(DEFAULT_FULL_SCAN_INTERVAL)
        .clamp(DEFAULT_INTERVAL, DEFAULT_FULL_SCAN_INTERVAL)
}

fn run_watched(invocation: &run::Invocation, out: &Sink, status: &Sink) {
    if let Err(error) = run::execute(invocation, out, status) {
        status.line(&error);
    }
}

fn rerun_if_changed(
    invocation: &run::Invocation,
    snapshot: &mut Snapshot,
    out: &Sink,
    status: &Sink,
) {
    let current = self::snapshot(invocation);
    if current == *snapshot {
        return;
    }
    *snapshot = current;
    let name = invocation
        .script
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    status.line(&format!("change detected, re-running {name}"));
    run_watched(invocation, out, status);
}

/// Stamps the script and every `.vibe` file under the module directories,
/// which are walked recursively because `require` resolves nested paths.
/// Linked directories are not descended; linked files are stamped through
/// their link, so a dangling link's target appearing registers as a change.
fn snapshot(invocation: &run::Invocation) -> Snapshot {
    let mut snapshot = Snapshot::new();
    let script = resolve(&invocation.script);
    let script_stamp = stamp(&script);
    snapshot.insert(script, script_stamp);
    for directory in &invocation.module_dirs {
        let mut pending = vec![resolve(directory)];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                if is_dir {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "vibe") {
                    let stamp = stamp(&path);
                    snapshot.insert(path, stamp);
                }
            }
        }
    }
    snapshot
}

fn known_changed(snapshot: &Snapshot) -> bool {
    snapshot
        .iter()
        .any(|(path, stamp)| self::stamp(path) != *stamp)
}

/// Follows links like `stat`; any failure yields the empty stamp.
fn stamp(path: &Path) -> Stamp {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| crate::compat::clean(path))
}

// WASI preview 1 has no threads to run the watch loop beside the test.
#[cfg(all(test, not(target_os = "wasi")))]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.cache/tmp");
            fs::create_dir_all(&base).unwrap();
            let path = base.join(format!("watch-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn invocation(script: &Path, dir: &Path) -> run::Invocation {
        run::Invocation {
            script: script.to_owned(),
            function: Some("run".to_owned()),
            module_dirs: vec![dir.to_owned()],
            arguments: Vec::new(),
            limits: crate::profiles::limits(-1, -1, 10_000),
        }
    }

    fn text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned()
    }

    fn wait_for(buffer: &Arc<Mutex<Vec<u8>>>, want: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if text(buffer).matches(want).count() >= count {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "timed out waiting for {want:?} x{count}, got {:?}",
            text(buffer)
        );
    }

    /// Runs the watch loop on a thread until the returned guard stops it.
    struct Watching {
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<Result<(), String>>>,
        out: Arc<Mutex<Vec<u8>>>,
        status: Arc<Mutex<Vec<u8>>>,
    }

    impl Watching {
        fn start(invocation: run::Invocation) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let (out_sink, out) = Sink::buffer();
            let (status_sink, status) = Sink::buffer();
            let flag = stop.clone();
            let thread = thread::spawn(move || {
                watch(
                    &invocation,
                    Duration::from_millis(10),
                    &|| flag.load(Ordering::SeqCst),
                    &out_sink,
                    &status_sink,
                )
            });
            Self {
                stop,
                thread: Some(thread),
                out,
                status,
            }
        }

        fn finish(mut self) -> String {
            self.stop.store(true, Ordering::SeqCst);
            let result = self.thread.take().unwrap().join().unwrap();
            assert_eq!(result, Ok(()));
            text(&self.status)
        }
    }

    impl Drop for Watching {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn reruns_on_change_and_survives_errors() {
        let scratch = Scratch::new("rerun");
        let script = scratch.0.join("main.vibe");
        fs::write(&script, "def run -> string\n  \"first\"\nend\n").unwrap();
        let watching = Watching::start(invocation(&script, &scratch.0));
        wait_for(&watching.out, "first", 1);
        fs::write(&script, "def run -> string\n  \"second result\"\nend\n").unwrap();
        wait_for(&watching.out, "second result", 1);
        wait_for(&watching.status, "change detected, re-running main.vibe", 1);
        fs::write(&script, "def run(\n").unwrap();
        wait_for(&watching.status, "compile failed", 1);
        fs::write(&script, "def run -> string\n  \"recovered output\"\nend\n").unwrap();
        wait_for(&watching.out, "recovered output", 1);
        let status = watching.finish();
        assert!(
            status.starts_with("watching 1 file(s); press ctrl-c to stop\n"),
            "{status}"
        );
        assert!(status.ends_with("watch stopped\n"), "{status}");
    }

    #[test]
    fn reruns_on_module_changes_additions_and_deletions() {
        let scratch = Scratch::new("modules");
        let script = scratch.0.join("main.vibe");
        fs::write(&script, "def run -> string\n  \"module watch up\"\nend\n").unwrap();
        fs::create_dir(scratch.0.join("billing")).unwrap();
        let module = scratch.0.join("billing").join("helper.vibe");
        fs::write(&module, "def helper()\n  1\nend\n").unwrap();
        let watching = Watching::start(invocation(&script, &scratch.0));
        wait_for(&watching.out, "module watch up", 1);
        fs::write(&module, "def helper()\n  22\nend\n").unwrap();
        wait_for(&watching.status, "change detected", 1);
        let nested = scratch.0.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("fees.vibe"), "def fees()\n  2\nend\n").unwrap();
        wait_for(&watching.status, "change detected", 2);
        fs::remove_file(&module).unwrap();
        wait_for(&watching.status, "change detected", 3);
        watching.finish();
    }

    #[test]
    fn snapshots_stamp_vibe_files_recursively() {
        let scratch = Scratch::new("snapshot");
        let dir = &scratch.0;
        let script = dir.join("main.vibe");
        fs::write(&script, "def run()\n  nil\nend\n").unwrap();
        fs::write(dir.join("helper.vibe"), "def helper()\n  1\nend\n").unwrap();
        fs::write(dir.join("notes.txt"), "ignored").unwrap();
        fs::create_dir_all(dir.join("billing/deep")).unwrap();
        fs::write(dir.join("billing/fees.vibe"), "def fees()\n  2\nend\n").unwrap();
        fs::write(
            dir.join("billing/deep/rates.vibe"),
            "def rates()\n  3\nend\n",
        )
        .unwrap();
        fs::write(dir.join("billing/readme.md"), "ignored").unwrap();
        fs::create_dir(dir.join("dir-named.vibe")).unwrap();
        let snapshot = snapshot(&invocation(&script, dir));
        let mut paths: Vec<_> = snapshot.keys().cloned().collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                dir.join("billing/deep/rates.vibe"),
                dir.join("billing/fees.vibe"),
                dir.join("helper.vibe"),
                dir.join("main.vibe"),
            ]
        );
    }

    #[test]
    fn known_file_edits_deletions_and_dangling_targets_register() {
        let scratch = Scratch::new("known");
        let dir = &scratch.0;
        let script = dir.join("main.vibe");
        fs::write(&script, "def run()\n  nil\nend\n").unwrap();
        let module = dir.join("helper.vibe");
        fs::write(&module, "def helper()\n  1\nend\n").unwrap();
        let snapshot = snapshot(&invocation(&script, dir));
        assert!(!known_changed(&snapshot));
        fs::write(&module, "def helper()\n  22\nend\n").unwrap();
        assert!(known_changed(&snapshot));
        let snapshot = self::snapshot(&invocation(&script, dir));
        fs::remove_file(&module).unwrap();
        assert!(known_changed(&snapshot));
        #[cfg(unix)]
        {
            let target = dir.join("missing-target.vibe");
            std::os::unix::fs::symlink(&target, dir.join("ghost.vibe")).unwrap();
            let snapshot = self::snapshot(&invocation(&script, dir));
            assert_eq!(snapshot.get(&dir.join("ghost.vibe")), Some(&None));
            fs::write(&target, "def helper()\n  1\nend\n").unwrap();
            assert!(known_changed(&snapshot));
        }
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_walk_a_linked_module_root() {
        let scratch = Scratch::new("linked");
        let real = scratch.0.join("real");
        fs::create_dir(&real).unwrap();
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let script = scratch.0.join("main.vibe");
        fs::write(&script, "def run()\n  nil\nend\n").unwrap();
        let helper = real.join("helper.vibe");
        fs::write(&helper, "def helper()\n  1\nend\n").unwrap();
        let snapshot = snapshot(&invocation(&script, &link));
        assert!(snapshot.contains_key(&helper), "{snapshot:?}");
        fs::write(&helper, "def helper()\n  22\nend\n").unwrap();
        assert!(known_changed(&snapshot));
    }

    #[test]
    fn full_scans_are_bounded() {
        assert_eq!(
            full_scan_interval(Duration::from_millis(10)),
            Duration::from_millis(300)
        );
        assert_eq!(
            full_scan_interval(Duration::from_millis(100)),
            Duration::from_secs(2)
        );
        assert_eq!(
            full_scan_interval(DEFAULT_INTERVAL),
            DEFAULT_FULL_SCAN_INTERVAL
        );
        assert_eq!(
            full_scan_interval(Duration::MAX),
            DEFAULT_FULL_SCAN_INTERVAL
        );
    }
}
