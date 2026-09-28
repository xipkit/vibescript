//! `vibes run -watch` as a process: it re-runs on change and an interrupt stops it.

// Signals and subprocesses need Unix.
#![cfg(unix)]

mod support;
use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use support::{Files, VIBES};

fn collect(mut stream: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let text = Arc::new(Mutex::new(String::new()));
    let sink = text.clone();
    thread::spawn(move || {
        let mut buffer = [0; 256];
        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    });
    text
}

fn wait_for(text: &Arc<Mutex<String>>, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if text.lock().unwrap().contains(want) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for {want:?} in {:?}",
        text.lock().unwrap()
    );
}

#[test]
fn watch_reruns_on_change_and_stops_on_interrupt() {
    let files = Files::new();
    let script = files.write("main.vibe", "def run -> string\n  \"first\"\nend\n");
    files.write("helper.vibe", "def helper\n  1\nend\n");
    let mut child = Command::new(VIBES)
        .args(["run", "-watch", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = collect(child.stdout.take().unwrap());
    let stderr = collect(child.stderr.take().unwrap());
    wait_for(&stderr, "watching 2 file(s); press ctrl-c to stop\n");
    wait_for(&stdout, "first\n");
    // A distinct size guarantees a new stamp on coarse clocks.
    fs::write(&script, "def run -> string\n  \"second result\"\nend\n").unwrap();
    wait_for(&stdout, "second result\n");
    wait_for(&stderr, "change detected, re-running main.vibe\n");
    fs::write(&script, "def run(\n").unwrap();
    wait_for(&stderr, "error[V0001]: ");
    // SAFETY: the pid names the child process this test spawned and still owns.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    wait_for(&stderr, "watch stopped\n");
}
