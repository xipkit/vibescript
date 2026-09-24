//! Line mode: drives the REPL from a pipe or file, one line per input, and
//! prints each transcript entry as the terminal front end would draw it.

use super::{
    model::{self, Command, Model},
    render::{self, Colors, Line},
};
use std::io::{self, BufRead, Write};

/// Reads lines until `:quit`, a Ctrl-D byte or the end of input, submitting
/// each as Enter would. A line ends at `\n`, `\r` or `\r\n`. A Ctrl-C byte
/// discards an unfinished input, or quits when there is none. An input still
/// unfinished at the end is evaluated so its error is reported.
pub fn run(
    model: &mut Model,
    mut input: impl BufRead,
    mut output: impl Write,
    colors: Colors,
) -> io::Result<()> {
    let mut printed = 0;
    let mut line = Vec::new();
    let mut after_cr = false;
    loop {
        if model.session.is_cancelled() {
            return Err(io::Error::other("execution cancelled"));
        }
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let mut consumed = 0;
        let mut command = Command::Continue;
        for &byte in buffer {
            consumed += 1;
            let skip = after_cr && byte == b'\n';
            after_cr = byte == b'\r';
            command = match byte {
                _ if skip => Command::Continue,
                b'\n' | b'\r' => {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    let command = model.submit(&text);
                    show(model, &mut printed, &mut output, colors)?;
                    command
                }
                0x04 => Command::Quit,
                0x03 => {
                    line.clear();
                    model.update(model::Key::CtrlC)
                }
                byte => {
                    line.push(byte);
                    Command::Continue
                }
            };
            if command == Command::Quit {
                break;
            }
        }
        input.consume(consumed);
        if command == Command::Quit {
            return output.flush();
        }
    }
    if !line.is_empty() {
        let text = String::from_utf8_lossy(&line).into_owned();
        if model.submit(&text) == Command::Quit {
            show(model, &mut printed, &mut output, colors)?;
            return output.flush();
        }
    }
    model.session.finish();
    show(model, &mut printed, &mut output, colors)?;
    output.flush()
}

/// Prints transcript entries added since the last call, then any panel the
/// last command turned on.
fn show(
    model: &mut Model,
    printed: &mut usize,
    output: &mut impl Write,
    colors: Colors,
) -> io::Result<()> {
    let transcript = model.session.transcript();
    *printed = (*printed).min(transcript.len());
    let mut lines: Vec<Line> = Vec::new();
    for entry in &transcript[*printed..] {
        lines.extend(model::entry_lines(entry));
        lines.push(Vec::new());
    }
    *printed = transcript.len();
    if std::mem::take(&mut model.show_vars) {
        lines.extend(model.vars_panel());
    }
    if std::mem::take(&mut model.show_help) {
        lines.extend(model::help_panel());
    }
    let mut text = String::new();
    for line in &lines {
        render::ansi(&mut text, line, colors, None);
        text.push('\n');
    }
    output.write_all(text.as_bytes())
}
