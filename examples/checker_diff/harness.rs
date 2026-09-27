//! Runs one program the way a release build does and with every proven type
//! check kept ([`Engine::set_keep_type_checks`]), and judges whether the
//! checker's verdict agrees with what happened.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Limits, ModuleConfig, Value};

/// A program: the script and the files it may require, by relative path.
#[derive(Clone, Debug, Default)]
pub struct Case {
    pub main: String,
    pub modules: Vec<(String, String)>,
}

impl Case {
    /// A program without required files.
    pub fn new(main: impl Into<String>) -> Self {
        Self {
            main: main.into(),
            modules: Vec::new(),
        }
    }

    /// The program as one text: each required file under a `#@ file PATH`
    /// line, then the script under `#@ main`.
    pub fn render(&self) -> String {
        let mut text = String::new();
        for (path, source) in &self.modules {
            text.push_str(&format!("#@ file {path}\n{source}"));
            if !source.ends_with('\n') {
                text.push('\n');
            }
        }
        if !self.modules.is_empty() {
            text.push_str("#@ main\n");
        }
        text.push_str(&self.main);
        text
    }

    /// Reads [`Self::render`]'s form back, after any leading comment lines
    /// that describe it.
    pub fn parse(text: &str) -> Self {
        let mut text = text;
        while text.starts_with("# ") {
            text = text.split_once('\n').map_or("", |(_, rest)| rest);
        }
        if !text.starts_with("#@ ") {
            return Self::new(text);
        }
        let mut case = Self::default();
        let mut current: Option<String> = None;
        let mut body = String::new();
        let finish =
            |current: &mut Option<String>, body: &mut String, case: &mut Case| match current.take()
            {
                Some(path) if path.is_empty() => case.main = std::mem::take(body),
                Some(path) => case.modules.push((path, std::mem::take(body))),
                None => body.clear(),
            };
        for line in text.split_inclusive('\n') {
            if let Some(path) = line.strip_prefix("#@ file ") {
                finish(&mut current, &mut body, &mut case);
                current = Some(path.trim().to_owned());
            } else if line.trim_end() == "#@ main" {
                finish(&mut current, &mut body, &mut case);
                current = Some(String::new());
            } else {
                body.push_str(line);
            }
        }
        finish(&mut current, &mut body, &mut case);
        case
    }
}

/// What checking and running a program showed.
#[derive(Clone, Debug)]
pub enum Verdict {
    /// The checker rejected the program: its first error's code, span and
    /// message.
    Rejected(String),
    /// Both builds ran it to the same observations, ending with this
    /// result.
    Agreed(String),
    /// A limit stopped one of the runs, so they cannot be compared.
    Inconclusive,
    /// The checker and the runtime disagree.
    Finding(Finding),
}

/// A disagreement between the checker and the runtime.
#[derive(Clone, Debug)]
pub struct Finding {
    pub kind: FindingKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindingKind {
    /// A check the checker proved failed with the checks kept.
    CheckFailed,
    /// The two builds observed different results or output.
    Mismatch,
    /// Checking, compiling or running panicked.
    Panic,
    /// Only one build compiled the program.
    CompileMismatch,
    /// Both builds raised an error the checker rules out, such as a call
    /// of a member the receiver does not have.
    Unexpected,
}

impl FindingKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::CheckFailed => "check-failed",
            Self::Mismatch => "mismatch",
            Self::Panic => "panic",
            Self::CompileMismatch => "compile-mismatch",
            Self::Unexpected => "unexpected-error",
        }
    }
}

/// One run's observations.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    result: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    limited: bool,
    type_error: bool,
    /// An error of a kind the checker rules out in a program it accepts.
    unexpected: bool,
}

/// Where required files are written, one directory per worker.
pub struct Scratch {
    root: std::path::PathBuf,
    next: usize,
}

impl Scratch {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            next: 0,
        }
    }

    /// A fresh, empty directory holding `modules`.
    fn directory(&mut self, modules: &[(String, String)]) -> std::io::Result<std::path::PathBuf> {
        self.next += 1;
        let path = self.root.join(format!("case-{}", self.next % 4));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        for (name, source) in modules {
            let file = path.join(name);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, source)?;
        }
        Ok(path)
    }
}

/// Checks `case`, then runs it in both builds and compares them.
pub fn judge(case: &Case, scratch: &mut Scratch) -> Verdict {
    let directory = if case.modules.is_empty() {
        None
    } else {
        match scratch.directory(&case.modules) {
            Ok(path) => Some(path),
            Err(error) => panic!("write required files: {error}"),
        }
    };
    let normal = match compile(case, directory.as_deref(), false) {
        Ok(Ok(script)) => script,
        Ok(Err(error)) => {
            return match compile(case, directory.as_deref(), true) {
                Ok(Ok(_)) => finding(
                    FindingKind::CompileMismatch,
                    format!("only the build with checks compiles; the other: {error}"),
                ),
                _ => Verdict::Rejected(first_code(&error)),
            };
        }
        Err(panic) => return finding(FindingKind::Panic, format!("compile: {panic}")),
    };
    let kept = match compile(case, directory.as_deref(), true) {
        Ok(Ok(script)) => script,
        Ok(Err(error)) => {
            return finding(
                FindingKind::CompileMismatch,
                format!("only the release build compiles; with checks: {error}"),
            );
        }
        Err(panic) => return finding(FindingKind::Panic, format!("compile with checks: {panic}")),
    };
    let first = match observe(&normal) {
        Ok(observation) => observation,
        Err(panic) => return finding(FindingKind::Panic, format!("run: {panic}")),
    };
    let second = match observe(&kept) {
        Ok(observation) => observation,
        Err(panic) => return finding(FindingKind::Panic, format!("run with checks: {panic}")),
    };
    if first.limited || second.limited {
        return Verdict::Inconclusive;
    }
    if first == second {
        if first.unexpected {
            return finding(FindingKind::Unexpected, first.result);
        }
        return Verdict::Agreed(first.result);
    }
    let kind = if second.type_error && !first.type_error {
        FindingKind::CheckFailed
    } else {
        FindingKind::Mismatch
    };
    let mut detail = format!("release: {}\nchecked: {}", first.result, second.result);
    for (name, a, b) in [
        ("stdout", &first.stdout, &second.stdout),
        ("stderr", &first.stderr, &second.stderr),
    ] {
        if a != b {
            detail.push_str(&format!(
                "\n{name} release: {:?}\n{name} checked: {:?}",
                String::from_utf8_lossy(a),
                String::from_utf8_lossy(b)
            ));
        }
    }
    finding(kind, detail)
}

/// The known difference `finding` shows, if it is one: a disagreement
/// reported and left for a language decision, which long runs count
/// instead of writing each one out (see `docs/checker-diff.md`).
pub fn known(case: &Case, finding: &Finding) -> Option<&'static str> {
    // What the build with checks saw, or the error both builds raised.
    let checked = match finding.kind {
        FindingKind::CheckFailed => finding
            .detail
            .lines()
            .find_map(|line| line.strip_prefix("checked: "))
            .unwrap_or_default(),
        FindingKind::Unexpected => &finding.detail,
        _ => return None,
    };
    let sources =
        || std::iter::once(&case.main).chain(case.modules.iter().map(|(_, source)| source));
    // An integer to a negative power is a float the checker types as an int.
    if checked.contains("float") && sources().any(|source| source.contains("**")) {
        return Some("negative-power");
    }
    // A NaN compared with `<=>` gives nil, which the checker types as an int.
    if checked.contains("got nil") && sources().any(|source| source.contains("<=>")) {
        return Some("nan-comparison");
    }
    None
}

fn finding(kind: FindingKind, detail: String) -> Verdict {
    Verdict::Finding(Finding { kind, detail })
}

/// The program with each top-level local's first assignment declaring the
/// type the checker inferred for it, as `vibes repl` declares a session's
/// variables, so that the build keeping every check verifies each inferred
/// type; `None` when no local can be declared.
pub fn annotate(case: &Case, scratch: &mut Scratch) -> Option<Case> {
    let directory = if case.modules.is_empty() {
        None
    } else {
        Some(scratch.directory(&case.modules).ok()?)
    };
    let mut engine = Engine::new();
    if let Some(directory) = directory {
        let config = ModuleConfig {
            paths: vec![directory],
            ..ModuleConfig::default()
        };
        engine.set_module_config(config).ok()?;
    }
    let checked = catch_unwind(AssertUnwindSafe(|| engine.type_check(&case.main)))
        .ok()?
        .ok()?;
    let mut declared = std::collections::HashSet::new();
    let mut changed = false;
    let mut main = String::new();
    for line in case.main.split_inclusive('\n') {
        let name: String = line
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        let annotation = (!name.is_empty() && line[name.len()..].starts_with(" = "))
            .then(|| checked.locals.iter().find(|(local, _)| *local == name))
            .flatten()
            .filter(|(_, ty)| ty != "any" && !ty.contains("never"));
        match annotation {
            Some((_, ty)) if declared.insert(name.clone()) => {
                main.push_str(&format!("{name}: {ty}{}", &line[name.len()..]));
                changed = true;
            }
            _ => {
                declared.insert(name);
                main.push_str(line);
            }
        }
    }
    changed.then(|| Case {
        main,
        modules: case.modules.clone(),
    })
}

type Captured = Arc<Mutex<Vec<u8>>>;

struct Compiled {
    script: vibescript::Script,
    stdout: Captured,
    stderr: Captured,
}

fn compile(
    case: &Case,
    directory: Option<&Path>,
    keep: bool,
) -> Result<Result<Compiled, Error>, String> {
    let stdout = Captured::default();
    let stderr = Captured::default();
    let mut engine = Engine::new();
    engine.set_keep_type_checks(keep);
    if let Some(directory) = directory {
        let config = ModuleConfig {
            paths: vec![directory.to_owned()],
            ..ModuleConfig::default()
        };
        if let Err(error) = engine.set_module_config(config) {
            return Ok(Err(error));
        }
    }
    let seed = std::sync::atomic::AtomicU64::new(1);
    engine.set_random_source(move |_, output| {
        for chunk in output.chunks_mut(8) {
            let state = seed.fetch_add(0x9e37_79b9_7f4a_7c15, std::sync::atomic::Ordering::Relaxed);
            let bytes = super::rng::mix(state).to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(output.len())
    });
    let out = stdout.clone();
    engine.set_output_writer(move |_, bytes| {
        out.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    });
    let err = stderr.clone();
    engine.set_error_writer(move |_, bytes| {
        err.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    });
    catch_unwind(AssertUnwindSafe(|| engine.compile(&case.main)))
        .map(|compiled| {
            compiled.map(|script| Compiled {
                script,
                stdout,
                stderr,
            })
        })
        .map_err(panic_message)
}

fn observe(compiled: &Compiled) -> Result<Observation, String> {
    let options = CallOptions {
        limits: Limits {
            steps: Some(2_000_000),
            memory_bytes: Some(64 << 20),
            recursion: 128,
        },
        deadline: Some(Instant::now() + Duration::from_secs(10)),
        ..CallOptions::default()
    };
    let outcome =
        catch_unwind(AssertUnwindSafe(|| compiled.script.run(options))).map_err(panic_message)?;
    let (result, limited, type_error, unexpected) = match outcome {
        Ok(outcome) => (
            format!("ok {}", render(&outcome.value)),
            false,
            false,
            false,
        ),
        Err(error) => {
            let limited = matches!(
                error.kind,
                ErrorKind::Steps
                    | ErrorKind::Memory
                    | ErrorKind::Recursion
                    | ErrorKind::Deadline
                    | ErrorKind::Cancelled
            );
            let type_error = error.kind == ErrorKind::Type;
            let unexpected = unexpected(&error);
            (describe(&error), limited, type_error, unexpected)
        }
    };
    let stdout = std::mem::take(&mut *compiled.stdout.lock().unwrap());
    let stderr = std::mem::take(&mut *compiled.stderr.lock().unwrap());
    Ok(Observation {
        result,
        stdout,
        stderr,
        limited,
        type_error,
        unexpected,
    })
}

/// Whether the checker rules out `error` in a program it accepts: a call
/// of a member or a name that does not exist, an operator on operands it
/// does not take, or a typed boundary's check, other than those at the
/// edges the runtime still checks. A cast or a `JSON.parse_as` can fail,
/// and so can the result check of an instance method whose class has no
/// `initialize` to assign its properties, and the check of a hash whose
/// key type no string satisfies. Builtins also raise type errors for values
/// out of their range, such as an integer beyond 64 bits.
fn unexpected(error: &Error) -> bool {
    let message = String::from_utf8_lossy(error.message_bytes());
    match error.kind {
        ErrorKind::Name => true,
        ErrorKind::Type => {
            // The runtime checks a hash type whose keys are not strings,
            // which the checker admits (docs/vm.md).
            let keys =
                message.contains("expected hash<") && !message.contains("expected hash<string");
            let legitimate = message.starts_with("cast value expected")
                || message.starts_with("JSON.parse_as value expected")
                || (message.starts_with("return value for ") && message.contains("nil"))
                || keys;
            // `is_type?` validates its atom as a builtin validates a value.
            let confused = message.contains("operands")
                || message.contains(" expected ")
                || (message.contains("unknown") && !message.contains("type atom"))
                || message.contains("undefined");
            confused && !legitimate
        }
        _ => false,
    }
}

/// A value's kind and contents, exact for floats and bytes, without the
/// memory charges its debug form shows.
fn render(value: &Value) -> String {
    let mut text = String::new();
    render_into(value, &mut text, 0);
    text
}

fn render_into(value: &Value, text: &mut String, depth: usize) {
    use std::fmt::Write;
    if depth > 64 {
        text.push_str("...");
        return;
    }
    let kind = value.type_name();
    if let Some(items) = value.as_array() {
        text.push_str(kind);
        text.push('[');
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            render_into(item, text, depth + 1);
        }
        text.push(']');
    } else if let Some(entries) = value.as_hash() {
        text.push_str(kind);
        text.push('{');
        for (index, (key, item)) in entries.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            render_into(key, text, depth + 1);
            text.push_str(" => ");
            render_into(item, text, depth + 1);
        }
        text.push('}');
    } else if kind == "int" {
        let _ = write!(text, "int({value})");
    } else if let Some(float) = value.as_float().filter(|_| kind == "float") {
        let _ = write!(text, "float({float:?}/{:x})", float.to_bits());
    } else if let Some(bytes) = value.as_bytes() {
        let _ = write!(text, "{kind}({:?})", String::from_utf8_lossy(bytes));
    } else {
        let _ = write!(text, "{kind}({value})");
    }
}

fn describe(error: &Error) -> String {
    let at = error
        .diagnostic
        .as_ref()
        .map(|diagnostic| {
            format!(
                " at {}:{}",
                diagnostic.position.line, diagnostic.position.column
            )
        })
        .unwrap_or_default();
    format!(
        "error {:?} {:?}{at}: {}",
        error.kind,
        error.class().map(|class| class.name()),
        String::from_utf8_lossy(error.message_bytes())
    )
}

/// The code of the first error a failed compilation reports, then its
/// position and message.
fn first_code(error: &Error) -> String {
    error
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| {
            format!(
                "{} {:?}: {}",
                diagnostic.code, diagnostic.span, diagnostic.message
            )
        })
        .unwrap_or_else(|| format!("{:?} {error}", error.kind))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "panic".to_owned())
}
