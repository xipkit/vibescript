//! The terminal-facing half of the Go REPL's model: the input line, the help
//! and variables panels, key handling and the screen layout. Evaluation,
//! commands, completion and history belong to the library's
//! [`ReplSession`]; front ends turn keys into [`Key`] values and draw the lines
//! [`Model::view`] returns.

use super::{
    editor::{Edit, Editor},
    render::{self, Line, Style, span},
};
use vibescript_tools::repl::{Completion, Entry, ReplOptions, ReplSession, Response, render_value};

/// The prompt before the first line of an input.
pub const PROMPT: &str = "vibes> ";
/// The prompt before each continuation line of an unfinished input.
pub const CONTINUATION: &str = "  ...> ";
const PLACEHOLDER: &str = "type an expression...";

/// A key the REPL responds to, named after the keys the Go REPL binds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Key {
    /// Typed text.
    Text(String),
    /// Bracketed-paste content; each line break submits a line.
    Paste(String),
    Enter,
    Up,
    Down,
    Tab,
    /// Quits, or discards an unfinished input.
    CtrlC,
    /// Quits.
    CtrlD,
    /// Clears the transcript.
    CtrlL,
    /// Toggles the variables panel.
    CtrlV,
    /// Toggles the help panel.
    CtrlK,
    Edit(Edit),
}

/// What the front end should do after a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Continue,
    Quit,
}

/// The REPL as the terminal shows it: a library session, the input line and
/// which panels are open.
pub struct Model {
    pub session: ReplSession,
    pub editor: Editor,
    pub show_help: bool,
    pub show_vars: bool,
    pub quitting: bool,
}

impl Model {
    pub fn new(options: ReplOptions) -> Self {
        Self {
            session: ReplSession::new(options),
            editor: Editor::default(),
            show_help: false,
            show_vars: false,
            quitting: false,
        }
    }

    /// Handles one key.
    pub fn update(&mut self, key: Key) -> Command {
        match key {
            Key::CtrlC if self.session.discard_pending() => self.editor.clear(),
            Key::CtrlC | Key::CtrlD => {
                self.quitting = true;
                return Command::Quit;
            }
            Key::CtrlL => self.session.clear_transcript(),
            Key::CtrlV => self.show_vars = !self.show_vars,
            Key::CtrlK => self.show_help = !self.show_help,
            Key::Up => {
                if let Some(line) = self.session.history_back() {
                    self.recall(&line);
                }
            }
            Key::Down => {
                if let Some(line) = self.session.history_forward() {
                    self.recall(&line);
                }
            }
            Key::Tab => {
                if let Completion::Completed(text) = self.session.complete(&self.editor.value()) {
                    self.recall(&text);
                }
            }
            Key::Enter => {
                let line = self.editor.value();
                return self.submit(&line);
            }
            Key::Text(text) => self.editor.insert(&text),
            Key::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let mut lines = text.split('\n').peekable();
                while let Some(line) = lines.next() {
                    self.editor.insert(line);
                    if lines.peek().is_some() && self.update(Key::Enter) == Command::Quit {
                        return Command::Quit;
                    }
                }
            }
            Key::Edit(edit) => self.editor.edit(edit),
        }
        Command::Continue
    }

    fn recall(&mut self, line: &str) {
        self.editor.set_value(line);
        self.editor.cursor_end();
    }

    /// Submits a line, as Enter does, and applies what the session asks for.
    pub fn submit(&mut self, line: &str) -> Command {
        let response = self.session.feed_line(line);
        if matches!(response, Response::Ignored) {
            return Command::Continue;
        }
        self.editor.clear();
        match response {
            Response::ToggleHelp => self.show_help = !self.show_help,
            Response::ToggleVariables => self.show_vars = !self.show_vars,
            Response::Quit => {
                self.quitting = true;
                return Command::Quit;
            }
            _ => {}
        }
        Command::Continue
    }

    /// Lays out the screen for a terminal of `width` by `height` cells and
    /// returns the lines with the cursor's row and column. Older transcript
    /// lines scroll away so the input stays in view.
    pub fn view(&mut self, width: usize, height: usize) -> (Vec<Line>, (usize, usize)) {
        let mut top = header(width);
        let mut transcript = Vec::new();
        for entry in self.session.transcript() {
            transcript.extend(entry_lines(entry));
            transcript.push(Vec::new());
        }
        let mut bottom = Vec::new();
        if self.show_vars {
            bottom.extend(self.vars_panel());
        }
        if self.show_help {
            bottom.extend(help_panel());
        }
        let pending = self.session.pending();
        for (index, line) in pending.iter().enumerate() {
            let prompt = if index == 0 { PROMPT } else { CONTINUATION };
            bottom.push(vec![span(prompt, Style::Accent), span(line, Style::Plain)]);
        }
        let prompt = if pending.is_empty() {
            PROMPT
        } else {
            CONTINUATION
        };
        let placeholder = pending.is_empty();
        let cursor_row = bottom.len();
        let (text, column) = self.editor.window(width.saturating_sub(10));
        let mut input = vec![span(prompt, Style::Accent)];
        if text.is_empty() && placeholder {
            input.push(span(PLACEHOLDER, Style::Placeholder));
        } else {
            input.push(span(text, Style::Plain));
        }
        bottom.push(input);
        bottom.push(Vec::new());
        bottom.push(footer());
        let room = height.saturating_sub(top.len() + bottom.len());
        let skip = transcript.len().saturating_sub(room);
        top.extend(transcript.into_iter().skip(skip));
        let cursor_row = top.len() + cursor_row;
        top.extend(bottom);
        // When even the panels do not fit, keep the bottom of the screen.
        let cut = top.len().saturating_sub(height.max(1));
        let lines = top.split_off(cut);
        let cursor = (
            cursor_row.saturating_sub(cut),
            render::width(prompt) + column,
        );
        (lines, cursor)
    }

    /// The variables panel: each variable as `name = value`.
    pub fn vars_panel(&self) -> Vec<Line> {
        let variables = self.session.variables();
        if variables.is_empty() {
            return render::panel(vec![vec![span("No variables defined", Style::Muted)]]);
        }
        let mut content = vec![vec![span("Variables", Style::Accent)]];
        for (name, value) in variables {
            let text = render_value(value);
            let mut lines = text.split('\n');
            content.push(vec![
                span("  ", Style::Plain),
                span(name, Style::Highlight),
                span(
                    format!(" = {}", lines.next().unwrap_or_default()),
                    Style::Plain,
                ),
            ]);
            content.extend(lines.map(|line| vec![span(line, Style::Plain)]));
        }
        render::panel(content)
    }
}

/// The title, version and rule above the transcript.
fn header(width: usize) -> Vec<Line> {
    vec![
        vec![
            span(" ", Style::Plain),
            span("Vibescript REPL", Style::Accent),
            span("  ", Style::Plain),
            span(concat!("v", env!("CARGO_PKG_VERSION")), Style::Muted),
        ],
        vec![span(
            "─".repeat(width.saturating_sub(2).min(60)),
            Style::Muted,
        )],
        Vec::new(),
    ]
}

/// Renders a transcript entry: the input after a muted `›`, then the output
/// after `→`, or `✗` for a failure. Later lines of a multi-line output start
/// at the margin, as in the Go REPL.
pub fn entry_lines(entry: &Entry) -> Vec<Line> {
    let mut lines = Vec::new();
    if !entry.input.is_empty() {
        for (index, line) in entry.input.split('\n').enumerate() {
            let marker = if index == 0 { "  › " } else { "    " };
            lines.push(vec![span(marker, Style::Muted), span(line, Style::Plain)]);
        }
    }
    let (marker, style) = if entry.is_error {
        ("✗ ", Style::Error)
    } else {
        ("→ ", Style::Result)
    };
    let mut output = render::styled_lines(&format!("{marker}{}", entry.output), style);
    output[0].insert(0, span("  ", Style::Plain));
    lines.extend(output);
    lines
}

/// The help panel listing keys and commands.
pub fn help_panel() -> Vec<Line> {
    const HELP: [(&str, &str); 12] = [
        ("↑/↓", "Navigate command history"),
        ("Tab", "Autocomplete"),
        ("Enter", "Execute expression"),
        (":help", "Toggle this help"),
        (":vars", "Toggle variables panel"),
        (":globals", "Print current globals"),
        (":functions", "List callable functions"),
        (":types", "Show global value types"),
        (":clear", "Clear history"),
        (":reset", "Reset environment"),
        (":last_error", "Show previous error"),
        (":quit", "Exit REPL"),
    ];
    let mut content = vec![vec![span("Help", Style::Accent)]];
    for (key, description) in HELP {
        content.push(vec![
            span("  ", Style::Plain),
            span(format!("{key:<8}"), Style::Highlight),
            span("  ", Style::Plain),
            span(description, Style::Muted),
        ]);
    }
    render::panel(content)
}

/// The key hints under the input.
fn footer() -> Line {
    let mut line = Vec::new();
    for (key, description) in [
        ("ctrl+k", " help  "),
        ("ctrl+v", " vars  "),
        ("ctrl+l", " clear  "),
        ("ctrl+c", " quit"),
    ] {
        line.push(span(key, Style::Highlight));
        line.push(span(description, Style::Muted));
    }
    line
}
