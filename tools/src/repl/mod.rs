//! An interactive Vibescript session, as `vibes repl` runs it, without any
//! terminal.
//!
//! A [`ReplSession`] takes input one line at a time. Lines that leave a
//! construct open are held until it is complete; a complete input runs as a
//! top-level snippet. The variables it assigns and the functions, classes,
//! modules and enums it declares stay available to later inputs, and `_` holds
//! the last result. Instances made in one input still belong to their class in
//! the next, while class and module state starts afresh for each input. Results and errors are rendered as the Go REPL renders
//! them, and errors keep their structured diagnostics with positions in the
//! text that was typed. Lines starting with `:` run the REPL's commands.
//!
//! ```
//! use vibescript_tools::repl::{ReplOptions, ReplSession, Response};
//!
//! let mut session = ReplSession::new(ReplOptions::default());
//! let response = session.feed_line("def double(n)");
//! assert!(matches!(response, Response::NeedsMoreInput));
//! session.feed_line("  n * 2");
//! session.feed_line("end");
//! session.feed_line("x = 20");
//! let Response::Evaluated(evaluation) = session.feed_line("double(x) + 2") else {
//!     panic!("expected an evaluation");
//! };
//! assert_eq!(evaluation.output, "42");
//! assert_eq!(evaluation.result.unwrap().as_int(), Some(42));
//! let Response::Command(entry) = session.feed_line(":globals") else {
//!     panic!("expected command output");
//! };
//! assert_eq!(entry.output, "_ = 42\nx = 20");
//! ```

mod catalog;
mod format;
mod session;
#[cfg(test)]
mod tests;

pub use catalog::{COMMANDS, KEYWORDS};
pub use format::render_value;

use std::collections::BTreeMap;
use vibescript::{CancellationToken, DeclarationKind, Error, Limits, Value};

/// How a [`ReplSession`] evaluates input.
#[derive(Clone, Debug, Default)]
pub struct ReplOptions {
    /// The limits each input runs under. The default is the library's
    /// sandbox default; `vibes repl` selects the unlimited `xhigh` profile.
    pub limits: Limits,
    /// Cancelling this token stops the current and every later evaluation.
    pub cancellation: CancellationToken,
}

/// One transcript entry: an input and what the REPL showed for it. A listing
/// of completions has no input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub input: String,
    pub output: String,
    pub is_error: bool,
}

/// A complete input and its outcome.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// The input as evaluated, without surrounding whitespace.
    pub input: String,
    /// The text the REPL shows: printed output followed by the rendered
    /// result unless it is nil, or `compile error: ` or `runtime error: `
    /// followed by the rendered error.
    pub output: String,
    /// The result, or the error with diagnostics positioned in the typed text.
    pub result: Result<Value, Error>,
}

impl Evaluation {
    /// Reports whether the input failed to compile or run.
    pub fn is_error(&self) -> bool {
        self.result.is_err()
    }
}

/// What feeding a line did.
#[derive(Clone, Debug)]
pub enum Response {
    /// The line was blank and nothing was pending.
    Ignored,
    /// The input so far leaves a construct open; the next line continues it.
    NeedsMoreInput,
    /// A complete input ran, and was added to the transcript and history.
    Evaluated(Evaluation),
    /// A command such as `:globals` added this entry to the transcript.
    Command(Entry),
    /// `:help` asks the front end to show or hide its help.
    ToggleHelp,
    /// `:vars` asks the front end to show or hide the variables.
    ToggleVariables,
    /// `:clear` emptied the transcript.
    Cleared,
    /// `:quit` asks the front end to exit.
    Quit,
}

/// The result of completing the last word of an input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Completion {
    /// Nothing matches, or there is no word to complete.
    None,
    /// One name matches; this is the whole input with the word completed.
    Completed(String),
    /// Several names match, in sorted order. They are also listed in the
    /// transcript as `Completions: a, b`.
    Candidates(Vec<String>),
}

/// A REPL session: evaluation state, the transcript and the input history.
pub struct ReplSession {
    session: session::Session,
    catalog: catalog::Catalog,
    pending: Vec<String>,
    transcript: Vec<Entry>,
    history: Vec<String>,
    recalled: Option<usize>,
}

impl ReplSession {
    /// Creates an empty session.
    pub fn new(options: ReplOptions) -> Self {
        Self {
            session: session::Session::new(options.limits, options.cancellation),
            catalog: catalog::Catalog::new(),
            pending: Vec::new(),
            transcript: Vec::new(),
            history: Vec::new(),
            recalled: None,
        }
    }

    /// Feeds one line, as pressing Enter does.
    ///
    /// With nothing pending, a blank line is ignored and a line starting with
    /// `:` runs a command. Other lines accumulate until they form an input
    /// that does not end inside an open construct, such as a `def` without
    /// its `end` or an unclosed bracket or string; that input is evaluated.
    pub fn feed_line(&mut self, line: &str) -> Response {
        let source = if self.pending.is_empty() {
            let input = line.trim();
            if input.is_empty() {
                return Response::Ignored;
            }
            self.recalled = None;
            if input.starts_with(':') {
                return self.command(input);
            }
            line.to_owned()
        } else {
            self.recalled = None;
            self.pending.push(line.to_owned());
            self.pending.join("\n")
        };
        if self.session.is_incomplete(source.trim()) {
            if self.pending.is_empty() {
                self.pending.push(line.to_owned());
            }
            return Response::NeedsMoreInput;
        }
        self.pending.clear();
        Response::Evaluated(self.record(source.trim()))
    }

    /// Feeds text containing any number of lines, split at `\n`, `\r\n` or
    /// `\r`, and returns the response to each. A final line break ends the
    /// last line rather than adding an empty one.
    pub fn feed(&mut self, text: &str) -> Vec<Response> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text);
        text.split('\n').map(|line| self.feed_line(line)).collect()
    }

    /// Evaluates whatever input is still pending, so a session that ends in
    /// the middle of a construct reports why it was unfinished.
    pub fn finish(&mut self) -> Option<Evaluation> {
        let input = self.pending.join("\n").trim().to_owned();
        self.pending.clear();
        (!input.is_empty()).then(|| self.record(&input))
    }

    /// Discards the pending lines of an unfinished input and reports whether
    /// there were any.
    pub fn discard_pending(&mut self) -> bool {
        self.recalled = None;
        !std::mem::take(&mut self.pending).is_empty()
    }

    /// The lines of the unfinished input, oldest first.
    pub fn pending(&self) -> &[String] {
        &self.pending
    }

    /// Evaluates one complete input without adding it to the transcript or
    /// history. The session's variables, declarations and last error are
    /// updated as for any input.
    pub fn evaluate(&mut self, input: &str) -> Evaluation {
        let evaluated = self.session.evaluate(input);
        Evaluation {
            input: input.to_owned(),
            output: evaluated.output,
            result: evaluated.result,
        }
    }

    fn record(&mut self, input: &str) -> Evaluation {
        let evaluation = self.evaluate(input);
        self.transcript.push(Entry {
            input: evaluation.input.clone(),
            output: evaluation.output.clone(),
            is_error: evaluation.is_error(),
        });
        self.history.push(evaluation.input.clone());
        evaluation
    }

    /// Runs a REPL command, named by the first word of `input`:
    ///
    /// | Command | Effect |
    /// | --- | --- |
    /// | `:help`, `:h` | [`Response::ToggleHelp`] |
    /// | `:vars`, `:v` | [`Response::ToggleVariables`] |
    /// | `:globals`, `:g` | lists variables as `name = value` |
    /// | `:functions`, `:f` | lists callable builtins, functions and variables |
    /// | `:types`, `:t` | lists variables as `name: type` |
    /// | `:clear`, `:c` | empties the transcript |
    /// | `:reset`, `:r` | forgets every variable and declaration |
    /// | `:last_error`, `:le` | shows the most recent error |
    /// | `:quit`, `:q` | [`Response::Quit`] |
    ///
    /// Commands are not added to the input history. An unknown command adds
    /// an error entry to the transcript.
    pub fn command(&mut self, input: &str) -> Response {
        let name = input.split_whitespace().next().unwrap_or_default();
        let entry = |output: String, is_error: bool| Entry {
            input: input.to_owned(),
            output,
            is_error,
        };
        let entry = match name {
            ":help" | ":h" => return Response::ToggleHelp,
            ":vars" | ":v" => return Response::ToggleVariables,
            ":quit" | ":q" => return Response::Quit,
            ":clear" | ":c" => {
                self.transcript.clear();
                return Response::Cleared;
            }
            ":globals" | ":g" => entry(self.globals(), false),
            ":functions" | ":f" => entry(self.functions(), false),
            ":types" | ":t" => entry(self.types(), false),
            ":reset" | ":r" => {
                self.session.reset();
                entry("Environment reset".to_owned(), false)
            }
            ":last_error" | ":le" => match self.last_error() {
                Some(error) => entry(error.to_owned(), true),
                None => entry("No previous error".to_owned(), false),
            },
            _ => entry(format!("Unknown command: {name}"), true),
        };
        self.transcript.push(entry.clone());
        Response::Command(entry)
    }

    /// Lists every variable as `name = value`, sorted by name.
    pub fn globals(&self) -> String {
        if self.session.env.is_empty() {
            return "No globals defined".to_owned();
        }
        let lines: Vec<_> = self
            .session
            .env
            .iter()
            .map(|(name, value)| format!("{name} = {}", render_value(value)))
            .collect();
        lines.join("\n")
    }

    /// Lists the callable builtins, the session's functions and the callable
    /// variables, one per line and sorted.
    pub fn functions(&self) -> String {
        let mut names = self.catalog.functions.clone();
        names.extend(
            self.declarations()
                .filter(|(kind, _)| *kind == DeclarationKind::Function)
                .map(|(_, name)| name.to_owned()),
        );
        for (name, value) in &self.session.env {
            if catalog::callable(value) {
                names.push(name.clone());
            }
        }
        names.sort();
        names.join("\n")
    }

    /// Lists every variable's type as `name: type`, sorted by name.
    pub fn types(&self) -> String {
        if self.session.env.is_empty() {
            return "No globals defined".to_owned();
        }
        let lines: Vec<_> = self
            .session
            .env
            .iter()
            .map(|(name, value)| format!("{name}: {}", value.type_name()))
            .collect();
        lines.join("\n")
    }

    /// Completes the last word of `input`: commands after `:`, qualified
    /// builtins such as `JSON.parse` after a dot, otherwise builtins and
    /// keywords, and always the session's variables and declarations.
    pub fn complete(&mut self, input: &str) -> Completion {
        if input.ends_with(char::is_whitespace) {
            return Completion::None;
        }
        let Some(word) = input.split_whitespace().last() else {
            return Completion::None;
        };
        let commands = catalog::COMMANDS.iter().copied();
        let builtins = if word.contains('.') {
            &self.catalog.documented
        } else {
            &self.catalog.top_level
        };
        let names: Box<dyn Iterator<Item = &str>> = if word.starts_with(':') {
            Box::new(commands)
        } else {
            Box::new(
                builtins
                    .iter()
                    .map(String::as_str)
                    .chain(catalog::KEYWORDS)
                    .chain(self.declarations().map(|(_, name)| name)),
            )
        };
        let mut matches: Vec<String> = names
            .chain(self.session.env.keys().map(String::as_str))
            .filter(|name| name.starts_with(word))
            .map(str::to_owned)
            .collect();
        matches.sort_unstable();
        matches.dedup();
        match matches.len() {
            0 => Completion::None,
            1 => Completion::Completed(format!(
                "{}{}",
                &input[..input.len() - word.len()],
                matches[0]
            )),
            _ => {
                self.transcript.push(Entry {
                    input: String::new(),
                    output: format!("Completions: {}", matches.join(", ")),
                    is_error: false,
                });
                Completion::Candidates(matches)
            }
        }
    }

    /// Steps back through the input history, as Up does. The recalled
    /// input's earlier lines become pending and its last line is returned
    /// for editing. Returns `None` when the history is empty.
    pub fn history_back(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        let index = self
            .recalled
            .map_or(self.history.len() - 1, |index| index.saturating_sub(1));
        self.recalled = Some(index);
        Some(self.recall(index))
    }

    /// Steps forward through the input history, as Down does. Past the
    /// newest input it returns an empty line and clears the pending lines.
    /// Returns `None` when no input is recalled.
    pub fn history_forward(&mut self) -> Option<String> {
        let index = self.recalled?;
        if index + 1 < self.history.len() {
            self.recalled = Some(index + 1);
            Some(self.recall(index + 1))
        } else {
            self.recalled = None;
            self.pending.clear();
            Some(String::new())
        }
    }

    fn recall(&mut self, index: usize) -> String {
        let mut lines: Vec<String> = self.history[index].split('\n').map(str::to_owned).collect();
        let last = lines.pop().unwrap_or_default();
        self.pending = lines;
        last
    }

    /// The variables visible to the next input, including `_`, the last
    /// result.
    pub fn variables(&self) -> &BTreeMap<String, Value> {
        &self.session.env
    }

    /// The variables, for a host to add, change or remove bindings.
    pub fn variables_mut(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.session.env
    }

    /// The declarations available to later inputs: functions in the order
    /// they were made, then classes, modules and enums by name.
    pub fn declarations(&self) -> impl Iterator<Item = (DeclarationKind, &str)> {
        let functions = self
            .session
            .prelude
            .iter()
            .map(|carried| (carried.kind, carried.name.as_str()));
        let types = self
            .session
            .types
            .iter()
            .map(|(name, (kind, _))| (*kind, name.as_str()));
        functions.chain(types)
    }

    /// The most recent failure, as it was shown.
    pub fn last_error(&self) -> Option<&str> {
        Some(self.session.last_error.as_str()).filter(|error| !error.is_empty())
    }

    /// The transcript: every evaluation, command entry and completion
    /// listing since the last `:clear`.
    pub fn transcript(&self) -> &[Entry] {
        &self.transcript
    }

    /// Empties the transcript, as `:clear` does.
    pub fn clear_transcript(&mut self) {
        self.transcript.clear();
    }

    /// Every evaluated input, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Forgets every variable and declaration, as `:reset` does.
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// The callable builtin names `:functions` lists, qualified as
    /// `JSON.parse` for namespace members.
    pub fn builtin_functions(&self) -> &[String] {
        &self.catalog.functions
    }

    /// Returns the token that interrupts the evaluation in progress, or the
    /// next one; a fresh token replaces it once it has been used.
    pub fn interrupter(&self) -> CancellationToken {
        self.session.interrupter()
    }

    /// Reports whether the session's own cancellation token was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.session.cancelled()
    }
}
