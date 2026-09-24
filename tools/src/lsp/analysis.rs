//! Compiles and checks a document, producing diagnostics in the reference's
//! form and the declaration outline navigation uses.

use super::document::{Diagnostic, Options, Position, Range, Severity};
use super::text::{line_at, utf16_character};
use std::path::PathBuf;
use std::time::Instant;
use vibescript::tooling::{self, Outline};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Limits, ModuleConfig};

/// A diagnostic whose range always moves forward: an empty or inverted range
/// covers one character from its start, as in the reference.
pub(crate) fn diagnostic(
    mut range: Range,
    severity: Severity,
    message: impl Into<String>,
) -> Diagnostic {
    let end = range.end;
    if end.line < range.start.line
        || (end.line == range.start.line && end.character <= range.start.character)
    {
        range.end = Position {
            line: range.start.line,
            character: range.start.character + 1,
        };
    }
    Diagnostic {
        range,
        severity,
        message: message.into(),
    }
}

/// What a fresh parse means for the cached navigation program.
pub(crate) enum Program {
    /// The source parsed; this outline replaces the cached one.
    Parsed(Outline),
    /// A syntax error: keep the last outline, re-anchored to the live buffer.
    Kept,
    /// The source could not be parsed at all, such as an oversized buffer:
    /// drop the cached outline and compiled facts.
    Missing,
}

pub(crate) struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    /// Whether the source compiled, making the parsed outline its compiled facts.
    pub compiled: bool,
    pub program: Program,
    /// Whether cancellation stopped the check before it finished.
    pub cancelled: bool,
}

/// Compiles the source, checks it when it compiles, and outlines it.
///
/// Required files resolve against the document's directory for file URIs,
/// as `vibes check FILE` resolves them; only diagnostics in the document
/// itself are reported.
pub(crate) fn analyze(uri: &str, source: &str, options: &Options) -> Analysis {
    if source.len() > options.max_source_bytes {
        return Analysis {
            diagnostics: vec![diagnostic(
                Range::default(),
                Severity::Error,
                format!(
                    "source exceeds maximum size ({} > {} bytes)",
                    source.len(),
                    options.max_source_bytes
                ),
            )],
            compiled: false,
            program: Program::Missing,
            cancelled: false,
        };
    }
    let deadline = Some(Instant::now() + options.timeout);
    let cancellation = options.cancellation.child_token();
    let engine = engine(uri, options);
    // Compilation shares the check's deadline. Its work and memory grow with
    // the source, which the size limit already bounds, so no quota applies.
    let compile = CallOptions {
        limits: Limits {
            steps: None,
            memory_bytes: None,
            ..options.limits.clone()
        },
        cancellation: cancellation.clone(),
        deadline,
        ..CallOptions::default()
    };
    let script = match engine.compile_with_options(source, &compile) {
        Ok(script) => script,
        Err(error) if error.kind == ErrorKind::Cancelled => return Analysis::cancelled(),
        Err(error) if matches!(error.kind, ErrorKind::Deadline | ErrorKind::Memory) => {
            return Analysis {
                diagnostics: vec![stopped("compilation", &error)],
                compiled: false,
                program: Program::Kept,
                cancelled: false,
            };
        }
        Err(error) => {
            let program = match tooling::outline(source) {
                Ok(outline) => Program::Parsed(outline),
                Err(error) if error.diagnostic.is_none() => Program::Missing,
                Err(_) => Program::Kept,
            };
            return Analysis {
                diagnostics: vec![compile_diagnostic(source, &error)],
                compiled: false,
                program,
                cancelled: false,
            };
        }
    };
    let program = match tooling::outline(source) {
        Ok(outline) => Program::Parsed(outline),
        Err(_) => Program::Kept,
    };
    let call = CallOptions {
        limits: options.limits.clone(),
        cancellation,
        deadline,
        ..CallOptions::default()
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let mut diagnostics = Vec::new();
    let mut cancelled = false;
    match script.check(&call) {
        Ok(report) => {
            let entries = [
                (&report.diagnostics, Severity::Error),
                (&report.incomplete, Severity::Information),
            ];
            for (entries, severity) in entries {
                for entry in entries.iter().filter(|entry| entry.filename.is_none()) {
                    let range = issue_range(&lines, entry.position.line, entry.position.column);
                    diagnostics.push(diagnostic(range, severity, entry.message.clone()));
                }
            }
        }
        Err(error) if error.kind == ErrorKind::Cancelled => cancelled = true,
        Err(error) => diagnostics.push(stopped("static check", &error)),
    }
    Analysis {
        diagnostics,
        compiled: true,
        program,
        cancelled,
    }
}

impl Analysis {
    fn cancelled() -> Self {
        Self {
            diagnostics: Vec::new(),
            compiled: false,
            program: Program::Kept,
            cancelled: true,
        }
    }
}

/// A warning that analysis stopped at a limit, so errors may be missing.
fn stopped(stage: &str, error: &Error) -> Diagnostic {
    diagnostic(
        Range::default(),
        Severity::Warning,
        format!("{stage} stopped: {}", error.message),
    )
}

/// An engine whose required files resolve from the configured directories,
/// or else from the document's directory.
fn engine(uri: &str, options: &Options) -> Engine {
    let mut engine = Engine::new();
    let paths = options.module_paths.clone().unwrap_or_else(|| {
        file_path(uri)
            .and_then(|path| path.parent().map(PathBuf::from))
            .filter(|directory| directory.is_dir())
            .into_iter()
            .collect()
    });
    if !paths.is_empty() {
        // Unreadable directories leave required files unresolved.
        let _ = engine.set_module_config(ModuleConfig {
            paths,
            ..ModuleConfig::default()
        });
    }
    // Checking never runs output helpers, but scripts may still name them.
    engine.set_output_writer(|_, _| Ok(()));
    engine.set_error_writer(|_, _| Ok(()));
    engine
}

/// The diagnostic for a compile failure: the error's position, or the
/// document start for errors without one, such as an oversized source.
fn compile_diagnostic(source: &str, error: &Error) -> Diagnostic {
    let Some(located) = &error.diagnostic else {
        return diagnostic(Range::default(), Severity::Error, error.message.clone());
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let range = issue_range(&lines, located.position.line, located.position.column);
    diagnostic(range, Severity::Error, error.message.clone())
}

/// Converts a one-based line and character column to an LSP range in UTF-16
/// units. Positions carry no end, so the range covers the identifier, number
/// or keyword starting there, or else one character.
pub(crate) fn issue_range(lines: &[&str], line: usize, column: usize) -> Range {
    let line_index = line.saturating_sub(1) as i64;
    let start = column.saturating_sub(1);
    let text = line_at(lines, line_index);
    let chars: Vec<char> = text.chars().collect();
    let mut end = start + 1;
    if chars
        .get(start)
        .is_some_and(|c| tooling::identifier_char(*c))
    {
        end = start;
        while chars.get(end).is_some_and(|c| tooling::identifier_char(*c)) {
            end += 1;
        }
        if chars.get(end).is_some_and(|c| matches!(c, '?' | '!')) {
            end += 1;
        }
    }
    let line = u32::try_from(line_index).unwrap_or(u32::MAX);
    let character = |column| u32::try_from(utf16_character(text, column)).unwrap_or(u32::MAX);
    Range {
        start: Position {
            line,
            character: character(start),
        },
        end: Position {
            line,
            character: character(end),
        },
    }
}

/// The local path of a `file:` URI, percent-decoded.
pub(crate) fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))?;
    let (authority, path) = rest.split_at(rest.find('/')?);
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or(path);
    native_path(percent_decode(path)?)
}

/// A decoded URI path as a native path: raw bytes on Unix, and `C:/dir` for
/// `/C:/dir` on Windows.
#[cfg(unix)]
fn native_path(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn native_path(bytes: Vec<u8>) -> Option<PathBuf> {
    let text = String::from_utf8(bytes).ok()?;
    let drive = text
        .strip_prefix('/')
        .filter(|rest| cfg!(windows) && rest.as_bytes().get(1) == Some(&b':'));
    Some(PathBuf::from(drive.unwrap_or(&text)))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        #[cfg(unix)]
        {
            assert_eq!(
                file_path("file:///tmp/My%20Project/caf%C3%A9.vibe"),
                Some(PathBuf::from("/tmp/My Project/café.vibe"))
            );
            assert_eq!(
                file_path("file://localhost/tmp/a.vibe"),
                Some(PathBuf::from("/tmp/a.vibe"))
            );
            assert_eq!(
                file_path("file:///tmp/%E6%97%A5.vibe"),
                Some(PathBuf::from("/tmp/日.vibe"))
            );
        }
        assert_eq!(file_path("untitled:Untitled-1"), None);
        assert_eq!(file_path("file://server/share/a.vibe"), None);
        assert_eq!(file_path("file:///tmp/%zz"), None);
    }

    #[test]
    fn issue_ranges_cover_the_word_at_the_position() {
        let lines = ["def 123()", "  x = [\"😀😀\", 1 2]", "  empty?(x) + 1"];
        let range = issue_range(&lines, 1, 5);
        assert_eq!((range.start.character, range.end.character), (4, 7));
        let range = issue_range(&lines, 2, 16);
        assert_eq!(
            (range.start.line, range.start.character, range.end.character),
            (1, 17, 18)
        );
        let range = issue_range(&lines, 2, 7);
        assert_eq!((range.start.character, range.end.character), (6, 7));
        let range = issue_range(&lines, 3, 3);
        assert_eq!((range.start.character, range.end.character), (2, 8));
        let range = issue_range(&lines, 9, 1);
        assert_eq!(
            (range.start.line, range.start.character, range.end.character),
            (8, 0, 1)
        );
    }
}
