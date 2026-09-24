//! The full-screen terminal front end: the Go REPL's alternate-screen layout,
//! redrawn after every key through crossterm.

use super::{
    editor::Edit,
    model::{Command, Key, Model},
    render::{self, Colors, Line, Style, span},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    collections::VecDeque,
    fmt::Write as _,
    io::{self, Write},
    time::{Duration, Instant},
};

/// How long an evaluation runs before the screen says it is busy.
const BUSY_AFTER: Duration = Duration::from_millis(150);

/// Runs the REPL on the terminal until it quits, restoring the terminal on
/// the way out, even after a panic.
pub fn run(model: &mut Model) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let _restore = Restore;
    let mut out = io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[?2004h")?;
    let colors = Colors::detect();
    let mut queued = VecDeque::new();
    loop {
        let (width, height) = size();
        let (lines, cursor) = model.view(width, height);
        draw(&mut out, &lines, cursor, colors, width)?;
        let key = match queued.pop_front() {
            Some(key) => key,
            None => match event::read()? {
                Event::Key(event) => match key(event) {
                    Some(key) => key,
                    None => continue,
                },
                Event::Paste(text) => Key::Paste(text),
                _ => continue,
            },
        };
        let command = if matches!(key, Key::Enter | Key::Paste(_)) {
            evaluate(model, key, &mut out, colors, &mut queued)?
        } else {
            model.update(key)
        };
        if command == Command::Quit {
            return Ok(());
        }
    }
}

/// Handles a key that may evaluate code on a worker thread, so Ctrl-C can
/// interrupt it. Keys typed meanwhile are kept for afterwards, as the Go
/// REPL's event queue keeps them.
fn evaluate(
    model: &mut Model,
    key: Key,
    out: &mut impl Write,
    colors: Colors,
    queued: &mut VecDeque<Key>,
) -> io::Result<Command> {
    let (width, height) = size();
    let (mut busy, cursor) = model.view(width, height);
    if let Some(last) = busy.last_mut() {
        *last = vec![span("evaluating… ctrl+c interrupts", Style::Muted)];
    }
    let interrupt = model.session.interrupter();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| model.update(key));
        let started = Instant::now();
        let mut shown = false;
        while !worker.is_finished() {
            if !shown && started.elapsed() >= BUSY_AFTER {
                draw(out, &busy, cursor, colors, width)?;
                shown = true;
            }
            if !event::poll(Duration::from_millis(20))? {
                continue;
            }
            match event::read()? {
                Event::Key(event) => match self::key(event) {
                    Some(Key::CtrlC) => interrupt.cancel(),
                    Some(key) => queued.push_back(key),
                    None => {}
                },
                Event::Paste(text) => queued.push_back(Key::Paste(text)),
                _ => {}
            }
        }
        // A panic keeps unwinding, so the terminal is restored on the way out.
        Ok(worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
    })
}

/// Maps a key press to the REPL's key, following the Go REPL's bindings and
/// the text input's editing keys.
fn key(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let control = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    Some(match event.code {
        KeyCode::Enter => Key::Enter,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab => Key::Tab,
        KeyCode::Left if control || alt => Key::Edit(Edit::WordLeft),
        KeyCode::Right if control || alt => Key::Edit(Edit::WordRight),
        KeyCode::Left => Key::Edit(Edit::Left),
        KeyCode::Right => Key::Edit(Edit::Right),
        KeyCode::Home => Key::Edit(Edit::Home),
        KeyCode::End => Key::Edit(Edit::End),
        KeyCode::Backspace if alt => Key::Edit(Edit::DeleteWordLeft),
        KeyCode::Backspace => Key::Edit(Edit::Backspace),
        KeyCode::Delete if alt => Key::Edit(Edit::DeleteWordRight),
        KeyCode::Delete => Key::Edit(Edit::Delete),
        KeyCode::Char(ch) if control => match ch.to_ascii_lowercase() {
            'c' => Key::CtrlC,
            'd' => Key::CtrlD,
            'l' => Key::CtrlL,
            'v' => Key::CtrlV,
            'k' => Key::CtrlK,
            'a' => Key::Edit(Edit::Home),
            'e' => Key::Edit(Edit::End),
            'b' => Key::Edit(Edit::Left),
            'f' => Key::Edit(Edit::Right),
            'h' => Key::Edit(Edit::Backspace),
            'u' => Key::Edit(Edit::DeleteToStart),
            'w' => Key::Edit(Edit::DeleteWordLeft),
            _ => return None,
        },
        KeyCode::Char(ch) if alt => match ch {
            'b' => Key::Edit(Edit::WordLeft),
            'f' => Key::Edit(Edit::WordRight),
            'd' => Key::Edit(Edit::DeleteWordRight),
            _ => return None,
        },
        KeyCode::Char(ch) => Key::Text(ch.to_string()),
        _ => return None,
    })
}

fn size() -> (usize, usize) {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    (usize::from(width), usize::from(height))
}

/// Draws a whole frame in one write, from the top-left corner, then places
/// the cursor.
fn draw(
    out: &mut impl Write,
    lines: &[Line],
    cursor: (usize, usize),
    colors: Colors,
    width: usize,
) -> io::Result<()> {
    let mut frame = String::from("\x1b[?2026h\x1b[?25l\x1b[H");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            frame.push_str("\r\n");
        }
        let start = frame.len();
        render::ansi(&mut frame, line, colors, Some(width));
        // A full-width line leaves the cursor on its last cell, which an
        // erase would clear.
        if render::width(&render::plain(line)) < width || frame.len() == start {
            frame.push_str("\x1b[K");
        }
    }
    let _ = write!(
        frame,
        "\x1b[J\x1b[{};{}H\x1b[?25h\x1b[?2026l",
        cursor.0 + 1,
        cursor.1 + 1
    );
    out.write_all(frame.as_bytes())?;
    out.flush()
}

/// Leaves the alternate screen and raw mode.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[?2004l\x1b[?1049l\x1b[?25h");
        let _ = out.flush();
        let _ = terminal::disable_raw_mode();
    }
}
