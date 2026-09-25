//! Evaluation for the REPL: compiles each input as a top-level snippet, runs it
//! with the session's variables as globals and keeps what it leaves behind.
//!
//! Variables, classes, modules and enums persist as the values
//! [`Script::run_bindings`] returns, passed back as globals, so instances keep
//! matching their classes. Functions are not values, so they persist as source:
//! each input is compiled after the function declarations carried from earlier
//! inputs, found through [`Script::declarations`], and positions are mapped
//! back to the text the user typed.

use super::format;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use vibescript::{
    CallOptions, CancellationToken, DeclarationKind, Diagnostic, Engine, Error, ErrorKind, Limits,
    Position, Script, Value,
};

/// The name reported for frames in the input, as the Go REPL does.
const DISPLAY_FUNCTION: &str = "<repl>";
/// The name the library gives top-level frames.
const TOP_LEVEL_FUNCTION: &str = "<script>";

/// Evaluation state that persists across inputs.
pub struct Session {
    engine: Engine,
    limits: Limits,
    cancellation: CancellationToken,
    interrupt: CancellationToken,
    /// Variables visible to the next input, including `_`, the last result.
    pub env: BTreeMap<String, Value>,
    /// Classes, modules and enums declared by earlier inputs, by name.
    pub types: BTreeMap<String, (DeclarationKind, Value)>,
    /// Functions declared by earlier inputs, in the order they were made.
    pub prelude: Vec<Carried>,
    /// The rendered text of the most recent failure, or empty.
    pub last_error: String,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

/// A top-level declaration kept for later inputs, with where it was typed.
#[derive(Clone, Debug)]
pub struct Carried {
    pub kind: DeclarationKind,
    pub name: String,
    text: String,
    line: usize,
    column: usize,
    source: Arc<str>,
}

impl Session {
    /// Creates a session whose inputs run under `limits`. Cancelling
    /// `cancellation` stops the current and every later evaluation.
    pub fn new(limits: Limits, cancellation: CancellationToken) -> Self {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut engine = Engine::new();
        // Each input sees the earlier inputs' variables as runtime globals,
        // which static types do not declare yet, so the session keeps the
        // ADR-004 language until it binds their types across inputs.
        engine.set_static_types(false);
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
        Self {
            engine,
            limits,
            interrupt: cancellation.child_token(),
            cancellation,
            env: BTreeMap::new(),
            types: BTreeMap::new(),
            prelude: Vec::new(),
            last_error: String::new(),
            stdout,
            stderr,
        }
    }

    /// Returns the token that interrupts the evaluation in progress, or the
    /// next one. Later evaluations get a fresh token once it has been used.
    pub fn interrupter(&self) -> CancellationToken {
        self.interrupt.clone()
    }

    /// Reports whether the whole session was cancelled.
    pub fn cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Forgets every variable and carried declaration.
    pub fn reset(&mut self) {
        self.env.clear();
        self.types.clear();
        self.prelude.clear();
    }

    /// Reports whether `source` ends inside an unfinished construct, so more
    /// lines should be read before evaluating it.
    pub fn is_incomplete(&self, source: &str) -> bool {
        match self.engine.compile(source) {
            // Like Go, an enum that ends its input before a member is
            // reported at its keyword.
            Err(error) if error.kind == ErrorKind::Syntax => {
                error.message.starts_with("unterminated")
                    || error.offset == Some(source.len())
                    || (error.message.ends_with("must define at least one member")
                        && source.split_whitespace().last() != Some("end"))
            }
            _ => false,
        }
    }

    /// Evaluates one input and returns its display text and result.
    ///
    /// Output written by `puts`, `print`, `p` and `warn` comes first, then the
    /// result unless it is nil. A failure is also kept as the last error, and
    /// leaves the variables and declarations unchanged.
    pub fn evaluate(&mut self, input: &str) -> Evaluated {
        self.stdout.lock().unwrap().clear();
        self.stderr.lock().unwrap().clear();
        let compiled = match self.compile(input) {
            Ok(compiled) => compiled,
            Err(error) => {
                let text = format!("compile error: {}", render_compile_error(&error));
                return self.fail(text, error);
            }
        };
        // A type the input declares again must not be shadowed by the old one.
        let redeclared = |name: &str| compiled.declared.iter().any(|new| new.name == name);
        let mut globals: BTreeMap<_, _> = self
            .types
            .iter()
            .filter(|(name, _)| !redeclared(name))
            .map(|(name, (_, value))| (name.clone(), value.clone()))
            .collect();
        globals.extend(self.env.clone());
        let options = CallOptions {
            globals,
            limits: self.limits.clone(),
            cancellation: self.interrupt.clone(),
            ..CallOptions::default()
        };
        let result = compiled.script.run_bindings(options);
        if self.interrupt.is_cancelled() {
            self.interrupt = self.cancellation.child_token();
        }
        match result {
            Ok((outcome, bindings)) => {
                let mut kinds: BTreeMap<String, DeclarationKind> = self
                    .types
                    .iter()
                    .filter(|(name, _)| !redeclared(name))
                    .map(|(name, (kind, _))| (name.clone(), *kind))
                    .collect();
                for declaration in &compiled.declared {
                    if declaration.kind != DeclarationKind::Function {
                        kinds.insert(declaration.name.clone(), declaration.kind);
                    }
                }
                self.env.clear();
                self.types.clear();
                for (name, value) in bindings {
                    match kinds.get(&name) {
                        Some(&kind) => {
                            self.types.insert(name, (kind, value));
                        }
                        None => {
                            self.env.insert(name, value);
                        }
                    }
                }
                self.prelude = compiled.functions;
                let output = self.output(&outcome.value);
                self.env.insert("_".to_owned(), outcome.value.clone());
                Evaluated {
                    output,
                    result: Ok(outcome.value),
                }
            }
            Err(error) => {
                let error = compiled.map.remap(error);
                self.fail(format!("runtime error: {error}"), error)
            }
        }
    }

    fn fail(&mut self, output: String, error: Error) -> Evaluated {
        self.last_error = output.clone();
        Evaluated {
            output,
            result: Err(error),
        }
    }

    fn output(&self, result: &Value) -> String {
        let mut captured = String::from_utf8_lossy(&self.stdout.lock().unwrap()).into_owned();
        captured.push_str(&String::from_utf8_lossy(&self.stderr.lock().unwrap()));
        let nil = result.type_name() == "nil";
        if captured.is_empty() {
            return if nil {
                "nil".to_owned()
            } else {
                format::render_value(result)
            };
        }
        if let Some(trimmed) = captured.strip_suffix('\n') {
            captured.truncate(trimmed.len());
        }
        if nil {
            captured
        } else if captured.is_empty() {
            format::render_value(result)
        } else {
            format!("{captured}\n{}", format::render_value(result))
        }
    }

    /// Compiles `input` after the carried functions it does not redeclare.
    /// Syntax errors are reported against the input alone.
    fn compile(&self, input: &str) -> Result<Compiled, Error> {
        let source: Arc<str> = Arc::from(input);
        let alone = match self.engine.compile(input) {
            Ok(script) => script,
            // A top-level alias can name a carried function, so the input may
            // need the prelude to compile at all.
            Err(error) => {
                return self
                    .compile_after_prelude(&source)
                    .ok_or_else(|| snippet_error(error, input.len()));
            }
        };
        let declared = carry(&source, alone.declarations(), 0);
        let kept = kept(&self.prelude, &declared);
        if kept.is_empty() {
            return Ok(Compiled::new(
                alone,
                SourceMap::plain(source),
                kept,
                declared,
            ));
        }
        let (combined, map) = SourceMap::build(&kept, source);
        let script = self
            .engine
            .compile(&combined)
            .map_err(|error| snippet_error(map.remap(error), combined.len()))?;
        Ok(Compiled::new(script, map, kept, declared))
    }

    /// Compiles the input after every carried function, taking its own
    /// declarations from the combined script. Returns `None` when that fails
    /// too, or when nothing is carried.
    fn compile_after_prelude(&self, source: &Arc<str>) -> Option<Compiled> {
        if self.prelude.is_empty() {
            return None;
        }
        let (combined, map) = SourceMap::build(&self.prelude, source.clone());
        let script = self.engine.compile(&combined).ok()?;
        let start = combined.len() - source.len();
        let own: Vec<_> = script
            .declarations()
            .iter()
            .filter(|declaration| declaration.span.start >= start)
            .cloned()
            .collect();
        let declared = carry(source, &own, start);
        let kept = kept(&self.prelude, &declared);
        Some(Compiled::new(script, map, kept, declared))
    }
}

/// The carried functions that `declared` does not replace.
fn kept(prelude: &[Carried], declared: &[Carried]) -> Vec<Carried> {
    prelude
        .iter()
        .filter(|carried| !declared.iter().any(|new| new.name == carried.name))
        .cloned()
        .collect()
}

struct Compiled {
    script: Script,
    map: SourceMap,
    /// The functions to carry after this input succeeds.
    functions: Vec<Carried>,
    /// Every top-level declaration in the input.
    declared: Vec<Carried>,
}

impl Compiled {
    fn new(script: Script, map: SourceMap, kept: Vec<Carried>, declared: Vec<Carried>) -> Self {
        let mut functions = kept;
        functions.extend(
            declared
                .iter()
                .filter(|carried| carried.kind == DeclarationKind::Function)
                .cloned(),
        );
        Self {
            script,
            map,
            functions,
            declared,
        }
    }
}

/// An evaluation's display text and its value or error.
pub struct Evaluated {
    pub output: String,
    pub result: Result<Value, Error>,
}

/// Records each declaration's text and starting position in `source`; spans
/// are offset by `base` bytes from the start of `source`.
fn carry(source: &Arc<str>, declarations: &[vibescript::Declaration], base: usize) -> Vec<Carried> {
    declarations
        .iter()
        .map(|declaration| {
            let start = declaration.span.start - base;
            let end = declaration.span.end - base;
            let before = &source[..start];
            let line = before.matches('\n').count() + 1;
            let column = before[before.rfind('\n').map_or(0, |i| i + 1)..]
                .chars()
                .count()
                + 1;
            Carried {
                kind: declaration.kind,
                name: declaration.name.clone(),
                text: source[start..end].to_owned(),
                line,
                column,
                source: source.clone(),
            }
        })
        .collect()
}

/// Maps positions in a compiled source back to the text the user typed.
struct SourceMap {
    input: Arc<str>,
    /// Lines before the input: the carried declarations.
    lines: usize,
    /// Each carried declaration and the combined-source line it starts on.
    regions: Vec<(usize, Carried)>,
}

impl SourceMap {
    fn plain(input: Arc<str>) -> Self {
        Self {
            input,
            lines: 0,
            regions: Vec::new(),
        }
    }

    /// Places each declaration on its own lines, indented to its original
    /// column so columns need no mapping, and then the input.
    fn build(prelude: &[Carried], input: Arc<str>) -> (String, Self) {
        let mut combined = String::new();
        let mut regions = Vec::new();
        let mut line = 1;
        for carried in prelude {
            regions.push((line, carried.clone()));
            combined.extend(std::iter::repeat_n(' ', carried.column - 1));
            combined.push_str(&carried.text);
            combined.push('\n');
            line += carried.text.matches('\n').count() + 1;
        }
        combined.push_str(&input);
        let map = Self {
            input,
            lines: line - 1,
            regions,
        };
        (combined, map)
    }

    /// Finds the typed text and position for a combined-source position.
    fn locate(&self, position: Position) -> (&str, Position) {
        if position.line > self.lines {
            let line = position.line - self.lines;
            return (&self.input, Position { line, ..position });
        }
        let (start, carried) = self
            .regions
            .iter()
            .rev()
            .find(|(start, _)| *start <= position.line)
            .unwrap_or(&self.regions[0]);
        let line = carried.line + position.line.saturating_sub(*start);
        (&carried.source, Position { line, ..position })
    }

    /// Rewrites an error's positions, code frame and top-level frame names.
    fn remap(&self, mut error: Error) -> Error {
        let Some(diagnostic) = error.diagnostic.as_deref() else {
            return error;
        };
        let mut diagnostic: Diagnostic = diagnostic.clone();
        if diagnostic.filename.is_none() && self.lines > 0 {
            let (text, position) = self.locate(diagnostic.position);
            diagnostic.code_frame = code_frame(text, position);
            diagnostic.position = position;
        }
        for frame in &mut diagnostic.frames {
            if frame.filename.is_none() {
                if self.lines > 0 {
                    frame.position = self.locate(frame.position).1;
                }
                if &*frame.function == TOP_LEVEL_FUNCTION {
                    frame.function = Arc::from(DISPLAY_FUNCTION);
                }
            }
        }
        error.diagnostic = Some(Arc::new(diagnostic));
        error
    }
}

/// Calls a parse error at the end of the input an unexpected end of snippet,
/// as the Go REPL does. `end` is the length of the compiled source.
fn snippet_error(mut error: Error, end: usize) -> Error {
    if error.kind == ErrorKind::Syntax
        && (error.offset == Some(end)
            || error.message.contains("end of source")
            || error.message.contains("end of input"))
    {
        error.message = "unexpected end of snippet".to_owned();
    }
    error
}

/// Renders a compile failure as `parse error at line:column: message` and
/// its code frame, as the Go REPL does.
fn render_compile_error(error: &Error) -> String {
    match error.diagnostic.as_ref() {
        Some(diagnostic) if error.kind == ErrorKind::Syntax => {
            let Position { line, column } = diagnostic.position;
            format!(
                "parse error at {line}:{column}: {}\n{}",
                error.message, diagnostic.code_frame
            )
        }
        _ => error.to_string(),
    }
}

/// Renders a code frame for `position` in `text`, in the library's format: a
/// location line, the source line clipped to a 160-character window, and a
/// caret that keeps the line's tabs.
fn code_frame(text: &str, position: Position) -> String {
    const WINDOW: usize = 160;
    let source_line = text
        .split('\n')
        .nth(position.line.saturating_sub(1))
        .unwrap_or_default();
    let chars: Vec<char> = source_line.chars().collect();
    let caret = (position.column.saturating_sub(1)).min(chars.len());
    let mut start = caret.saturating_sub(WINDOW / 2);
    let end = (start + WINDOW).min(chars.len());
    if end - start < WINDOW {
        start = end.saturating_sub(WINDOW);
    }
    let prefix = start > 0;
    let suffix = end < chars.len();
    let line = position.line;
    let mut frame = format!(
        "  --> line {line}, column {}\n {line} | {}{}{}\n ",
        position.column,
        if prefix { "..." } else { "" },
        chars[start..end].iter().collect::<String>(),
        if suffix { "..." } else { "" },
    );
    frame.extend(std::iter::repeat_n(' ', line.to_string().len()));
    frame.push_str(" | ");
    if prefix {
        frame.push_str("   ");
    }
    for &ch in &chars[start..caret] {
        frame.push(if ch == '\t' { '\t' } else { ' ' });
    }
    frame.push('^');
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_frames_match_the_library() {
        for (source, line) in [
            ("x = 1\n\tfoo(y)", 2),
            ("abc", 1),
            (&format!("{}zzz", "a".repeat(300)), 1),
        ] {
            // The session compiles without static types, so it matches
            // the library's frames for runtime errors.
            let mut engine = Engine::new();
            engine.set_static_types(false);
            let error = engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            let diagnostic = error.diagnostic.unwrap();
            assert_eq!(diagnostic.position.line, line);
            assert_eq!(
                code_frame(source, diagnostic.position),
                diagnostic.code_frame,
                "{source}"
            );
        }
    }
}
