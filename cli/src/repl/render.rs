//! Styled text for the REPL: lines of spans in named styles, the Go REPL's
//! colors, rounded panels, and ANSI output that degrades to the terminal's
//! color support.

use super::editor::char_width;
use std::fmt::Write;

/// A named style from the Go REPL's palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Style {
    Plain,
    /// Bold accent: the header title, the prompt and panel titles.
    Accent,
    /// Gray secondary text: the version, rule, input markers and help text.
    Muted,
    /// Green evaluation results.
    Result,
    /// Red failures.
    Error,
    /// Amber key names and variable names.
    Highlight,
    /// The accent color without bold: panel borders.
    Border,
    /// The input placeholder's gray.
    Placeholder,
}

/// Text in one style.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One screen line.
pub type Line = Vec<Span>;

/// Builds a span.
pub fn span(text: impl Into<String>, style: Style) -> Span {
    Span {
        text: text.into(),
        style,
    }
}

/// The plain text of a line, without styles.
pub fn plain(line: &[Span]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// The terminal columns a string occupies.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Splits styled text into lines, giving every line the same style, as
/// lipgloss does for a multi-line string.
pub fn styled_lines(text: &str, style: Style) -> Vec<Line> {
    text.split('\n')
        .map(|line| vec![span(line, style)])
        .collect()
}

/// Draws a rounded, accent-colored border with one column of padding around
/// `content`, widening every line to the widest.
pub fn panel(content: Vec<Line>) -> Vec<Line> {
    let inner = content
        .iter()
        .map(|line| width(&plain(line)))
        .max()
        .unwrap_or(0);
    let rule = "─".repeat(inner + 2);
    let mut lines = vec![vec![span(format!("╭{rule}╮"), Style::Border)]];
    for mut line in content {
        let pad = inner - width(&plain(&line));
        line.insert(0, span("│ ", Style::Border));
        line.push(span(" ".repeat(pad), Style::Plain));
        line.push(span(" │", Style::Border));
        lines.push(line);
    }
    lines.push(vec![span(format!("╰{rule}╯"), Style::Border)]);
    lines
}

/// How much color the terminal supports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Colors {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl Colors {
    /// Chooses a profile from the environment, as lipgloss does: `NO_COLOR`
    /// or a dumb terminal disables color, `COLORTERM` selects true color and
    /// a `256color` terminal selects the 256-color palette.
    pub fn detect() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        let term = var("TERM");
        if !var("NO_COLOR").is_empty() || term == "dumb" {
            return Self::None;
        }
        let colorterm = var("COLORTERM").to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            Self::TrueColor
        } else if term.contains("256color") {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }
}

/// An RGB color and the 256-color index and 16-color SGR code that lipgloss's
/// color profile converts it to.
#[derive(Clone, Copy)]
struct Color {
    rgb: (u8, u8, u8),
    ansi256: u8,
    ansi16: u8,
}

const ACCENT: Color = Color {
    rgb: (0x3B, 0x82, 0xF6),
    ansi256: 69,
    ansi16: 94,
};
const SUCCESS: Color = Color {
    rgb: (0x10, 0xB9, 0x81),
    ansi256: 36,
    ansi16: 32,
};
const ERROR: Color = Color {
    rgb: (0xEF, 0x44, 0x44),
    ansi256: 203,
    ansi16: 91,
};
const MUTED: Color = Color {
    rgb: (0x6B, 0x72, 0x80),
    ansi256: 60,
    ansi16: 34,
};
const HIGHLIGHT: Color = Color {
    rgb: (0xF5, 0x9E, 0x0B),
    ansi256: 214,
    ansi16: 91,
};

impl Style {
    fn parts(self) -> (Option<Color>, bool) {
        match self {
            Self::Plain => (None, false),
            Self::Accent => (Some(ACCENT), true),
            Self::Muted => (Some(MUTED), false),
            Self::Result => (Some(SUCCESS), false),
            Self::Error => (Some(ERROR), false),
            Self::Highlight => (Some(HIGHLIGHT), false),
            Self::Border => (Some(ACCENT), false),
            Self::Placeholder => (
                Some(Color {
                    rgb: (0x58, 0x58, 0x58),
                    ansi256: 240,
                    ansi16: 90,
                }),
                false,
            ),
        }
    }

    /// The escape sequence that starts this style, or nothing.
    fn open(self, colors: Colors) -> String {
        let (color, bold) = self.parts();
        let mut codes = Vec::new();
        if colors == Colors::None {
            return String::new();
        }
        if let Some(color) = color {
            codes.push(match colors {
                // The placeholder is an indexed color, which true-color
                // terminals receive unchanged.
                Colors::TrueColor if self == Self::Placeholder => {
                    format!("38;5;{}", color.ansi256)
                }
                Colors::TrueColor => {
                    let (r, g, b) = color.rgb;
                    format!("38;2;{r};{g};{b}")
                }
                Colors::Ansi256 => format!("38;5;{}", color.ansi256),
                _ => color.ansi16.to_string(),
            });
        }
        if bold {
            codes.push("1".to_owned());
        }
        if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        }
    }
}

/// Writes a line with escape sequences for `colors`, cut to `max` columns
/// when given.
pub fn ansi(out: &mut String, line: &[Span], colors: Colors, max: Option<usize>) {
    let mut used = 0;
    for span in line {
        let mut text = String::new();
        for ch in span.text.chars() {
            let columns = char_width(ch);
            if max.is_some_and(|max| used + columns > max) {
                break;
            }
            used += columns;
            text.push(ch);
        }
        if text.is_empty() {
            continue;
        }
        let open = span.style.open(colors);
        if open.is_empty() {
            out.push_str(&text);
        } else {
            let _ = write!(out, "{open}{text}\x1b[m");
        }
        if max.is_some_and(|max| used >= max) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_pad_every_line_inside_a_rounded_border() {
        let lines = panel(vec![
            vec![span("Help", Style::Accent)],
            vec![span("  ", Style::Plain), span("↑/↓", Style::Highlight)],
        ]);
        let text: Vec<_> = lines.iter().map(|line| plain(line)).collect();
        assert_eq!(text, ["╭───────╮", "│ Help  │", "│   ↑/↓ │", "╰───────╯"]);
    }

    #[test]
    fn escapes_follow_the_color_profile_and_width() {
        let line = vec![span("vibes> ", Style::Accent), span("1 + 2", Style::Plain)];
        let mut out = String::new();
        ansi(&mut out, &line, Colors::TrueColor, None);
        assert_eq!(out, "\x1b[38;2;59;130;246;1mvibes> \x1b[m1 + 2");
        out.clear();
        ansi(&mut out, &line, Colors::Ansi256, Some(9));
        assert_eq!(out, "\x1b[38;5;69;1mvibes> \x1b[m1 ");
        out.clear();
        ansi(&mut out, &line, Colors::None, Some(3));
        assert_eq!(out, "vib");
        out.clear();
        let placeholder = [span("type", Style::Placeholder)];
        ansi(&mut out, &placeholder, Colors::TrueColor, None);
        assert_eq!(out, "\x1b[38;5;240mtype\x1b[m");
        out.clear();
        ansi(&mut out, &placeholder, Colors::Ansi16, None);
        assert_eq!(out, "\x1b[90mtype\x1b[m");
    }
}
