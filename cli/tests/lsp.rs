// These tests run `vibes lsp` as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const VIBES: &str = env!("CARGO_BIN_EXE_vibes");

/// A running `vibes lsp` process driven in lockstep over its stdio.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: mpsc::Receiver<Value>,
}

impl Session {
    fn start() -> Self {
        Self::with_args(&[])
    }

    fn with_args(args: &[&str]) -> Self {
        let mut child = Command::new(VIBES)
            .arg("lsp")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("vibes lsp starts");
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            messages,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        self.write(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    }

    fn next(&self) -> Value {
        self.messages
            .recv_timeout(Duration::from_secs(120))
            .expect("the server replied")
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let reply = self.next();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Closes stdin and waits for the process, returning its status and stderr.
    fn finish(mut self) -> (Option<i32>, String) {
        drop(self.stdin.take());
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        let status = self.child.wait().unwrap();
        (status.code(), stderr)
    }
}

fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn position(uri: &str, line: i64, character: i64) -> Value {
    json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}})
}

/// A `file:` URI for a path, percent-encoding everything but unreserved bytes.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_str().unwrap().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'-' | b'_' | b'~' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// A fresh directory whose name has a space and a non-ASCII character.
fn directory(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../.cache/tmp")
        .join(format!("lsp {name} é {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

#[test]
fn serves_editor_features_over_stdio() {
    let directory = directory("session");
    std::fs::write(
        directory.join("helpers.vibe"),
        "export def double(x: int) -> int\n  x * 2\nend\n",
    )
    .unwrap();
    let uri = file_uri(&directory.join("main café.vibe"));
    let source = "helpers = require(\"helpers\")\n\n# Counts twice.\ndef twice(n: int) -> int\n  require(\"helpers\").double(n)\nend\n\ndef label(s: string) -> int\n  s.upcase\nend\n\ntwice(helpers.double(1))  \n";
    let mut session = Session::start();
    let initialize = session.request(1, "initialize", json!({"capabilities": {}}));
    assert_eq!(initialize["result"]["capabilities"]["textDocumentSync"], 1);
    session.notify("initialized", json!({}));

    session.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "vibescript", "version": 1, "text": source}}),
    );
    let published = session.next();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    assert_eq!(published["params"]["uri"], uri);
    // The required file resolves from the document's directory, so the only
    // finding is the checker's contradiction in label.
    assert_eq!(
        published["params"]["diagnostics"],
        json!([{
            "range": {"start": {"line": 8, "character": 2}, "end": {"line": 8, "character": 3}},
            "severity": 1,
            "source": "vibes-lsp",
            "message": "Return value: expected int, got string",
        }])
    );

    let hover = session.request(2, "textDocument/hover", position(&uri, 11, 1));
    assert_eq!(
        hover["result"]["contents"]["value"],
        "```vibe\ndef twice(n: int) -> int\n```\n\nCounts twice."
    );
    let completion = session.request(3, "textDocument/completion", position(&uri, 8, 10));
    let items = completion["result"]["items"].as_array().unwrap();
    assert!(items.iter().all(|item| item["detail"] == "string"));
    assert!(items.iter().any(|item| item["label"] == "upcase"));
    let definition = session.request(4, "textDocument/definition", position(&uri, 11, 2));
    assert_eq!(definition["result"]["uri"], uri);
    assert_eq!(
        definition["result"]["range"]["start"],
        json!({"line": 3, "character": 4})
    );
    let symbols = session.request(
        5,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    let names: Vec<&str> = symbols["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["twice", "label"]);
    let formatting = session.request(
        6,
        "textDocument/formatting",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(
        formatting["result"][0]["newText"],
        source.replace("))  \n", "))\n")
    );
    let signature = session.request(7, "textDocument/signatureHelp", position(&uri, 4, 19));
    assert_eq!(
        signature["result"]["signatures"][0]["label"],
        "require(module_name, as: nil) -> object"
    );
    assert_eq!(signature["result"]["activeParameter"], 0);

    session.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": format!("{source}def broken(\n")}]}),
    );
    let published = session.next();
    assert_eq!(
        published["params"]["diagnostics"].as_array().unwrap().len(),
        1
    );
    // Navigation keeps the declarations while the buffer does not parse.
    let definition = session.request(8, "textDocument/definition", position(&uri, 11, 2));
    assert_eq!(definition["result"]["range"]["start"]["line"], 3);

    session.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    let cleared = session.next();
    assert_eq!(cleared["params"]["diagnostics"], json!([]));
    let shutdown = session.request(9, "shutdown", Value::Null);
    assert_eq!(shutdown, json!({"jsonrpc": "2.0", "id": 9, "result": null}));
    session.notify("exit", Value::Null);
    let (status, stderr) = session.finish();
    assert_eq!(status, Some(0));
    assert_eq!(stderr, "");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn keeps_serving_after_malformed_messages() {
    let mut session = Session::start();
    session.write(b"Content-Length: 9\r\n\r\nnot json!");
    session.write(b"Content-Length: 5\r\n\r\n[1,2]");
    let mut oversized = format!("Content-Length: {}\r\n\r\n", (8 << 20) + 1).into_bytes();
    oversized.resize(oversized.len() + (8 << 20) + 1, b' ');
    session.write(&oversized);
    session.notify("$/cancelRequest", json!({"id": 99}));
    session.notify("workspace/didChangeConfiguration", json!({"settings": {}}));
    let unknown = session.request(1, "workspace/symbol", json!({"query": ""}));
    assert_eq!(
        unknown["error"],
        json!({"code": -32601, "message": "method not found"})
    );
    let invalid = session.request(2, "textDocument/hover", json!({"position": {"line": "x"}}));
    assert_eq!(
        invalid["error"],
        json!({"code": -32602, "message": "invalid hover params"})
    );
    let shutdown = session.request(3, "shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    session.notify("exit", Value::Null);
    let (status, stderr) = session.finish();
    assert_eq!((status, stderr.as_str()), (Some(0), ""));
}

#[test]
fn corrupt_framing_ends_the_server_with_an_error() {
    let mut session = Session::start();
    session.write(b"Content-Length: nope\r\n\r\n{}");
    let (status, stderr) = session.finish();
    assert_eq!(status, Some(1));
    assert_eq!(
        stderr,
        "lsp read: invalid Content-Length: strconv.Atoi: parsing \"nope\": invalid syntax\n"
    );
}

#[test]
fn the_server_exits_cleanly_when_the_client_closes_its_input() {
    let mut session = Session::start();
    session.request(1, "initialize", json!({}));
    let (status, stderr) = session.finish();
    assert_eq!((status, stderr.as_str()), (Some(0), ""));
}

#[test]
fn large_documents_stay_responsive() {
    let mut source = String::new();
    let mut functions = 0;
    while source.len() < 900 << 10 {
        source.push_str(&format!(
            "def f{functions}(x: int) -> int\n  y = x + {functions}\n  y * 2\nend\n\n"
        ));
        functions += 1;
    }
    let uri = "file:///tmp/large.vibe";
    let mut session = Session::start();
    let started = Instant::now();
    session.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let published = session.next();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    let hover = session.request(1, "textDocument/hover", position(uri, 0, 4));
    assert!(hover["result"]["contents"]["value"].is_string());
    // Analysis stops at its deadline, so even a debug build answers promptly.
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );

    let oversized = format!("{source}{}", "#\n".repeat(100_000));
    session.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri}, "contentChanges": [{"text": oversized}]}),
    );
    let published = session.next();
    let message = published["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.starts_with("source exceeds maximum size"),
        "{message}"
    );
    session.notify("exit", Value::Null);
    assert_eq!(session.finish().0, Some(0));
}

#[test]
fn lsp_takes_no_arguments() {
    // As in the reference: a usage error exits with status 1.
    let output = Command::new(VIBES).args(["lsp", "extra"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "vibes lsp: does not accept positional arguments\n"
    );
    let output = Command::new(VIBES)
        .args(["lsp", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "NAME:\n   vibes lsp - start the language server over stdio\n\n\
         USAGE:\n   vibes lsp [options]\n\n\
         OPTIONS:\n   --static    check documents in the static language (ADR-007), offering quick fixes\n   \
         --help, -h  show help\n"
    );
}

#[test]
fn the_static_server_publishes_codes_and_offers_quick_fixes() {
    let mut session = Session::with_args(&["--static"]);
    let initialized = session.request(1, "initialize", json!({}));
    assert_eq!(
        initialized["result"]["capabilities"]["codeActionProvider"],
        json!({"codeActionKinds": ["quickfix"]})
    );
    let uri = "file:///tmp/static-session.vibe";
    session.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "text": "x = nil\nputs 1 unless x.nil?\n"}}),
    );
    let published = session.next();
    let codes: Vec<&str> = published["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["V0407", "V0402"]);
    let actions = session.request(
        2,
        "textDocument/codeAction",
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 20}},
            "context": {"diagnostics": []},
        }),
    );
    let titles: Vec<&str> = actions["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["write `if` with the negated condition", "use `x == nil`"]
    );
    session.request(3, "shutdown", Value::Null);
    session.notify("exit", Value::Null);
    let (status, stderr) = session.finish();
    assert_eq!(status, Some(0), "{stderr}");
}
