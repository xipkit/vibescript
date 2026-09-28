//! Applies the machine-applicable fixes of compile diagnostics, rechecking
//! after each round, until none applies, as `vibes fix` does.
//!
//! Each round takes every diagnostic whose fix is
//! [`Applicability::Always`], innermost fix first, and applies those whose
//! edits neither overlap nor touch another's, so each fix meets the text it
//! was made for. The next round rechecks the result. A suggestion, which a
//! person should confirm, is never applied.
//!
//! ```
//! use vibescript_tools::fix::fix;
//! let engine = vibescript::Engine::new();
//! let fixed = fix("n = [1].size()\n", |text| {
//!     engine.type_check(text).map(|checked| checked.diagnostics)
//! })?;
//! assert_eq!(fixed.source, "n = [1].length\n");
//! assert_eq!(fixed.applied[0].diagnostic.code.to_string(), "V0401");
//! assert!(fixed.remaining.is_empty());
//! # Ok::<(), vibescript::Error>(())
//! ```

use std::collections::HashSet;
use vibescript::diagnostic::{Applicability, Diagnostic, Fix, Span};

/// The most rounds one source gets, a guard against fixes that undo each
/// other.
const ROUNDS: usize = 256;

/// A fix that was applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The diagnostic the fix repaired.
    pub diagnostic: Diagnostic,
    /// The one-based line and character column of the diagnostic in the
    /// text its round started from.
    pub line: usize,
    pub column: usize,
}

/// The result of fixing one source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixed {
    /// The source after every round.
    pub source: String,
    /// Every fix applied, round by round, in source order within a round.
    pub applied: Vec<Applied>,
    /// The diagnostics of the fixed source, in source order.
    pub remaining: Vec<Diagnostic>,
}

impl Fixed {
    /// Whether any fix changed the source.
    pub fn changed(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// Fixes `source`, where `check` returns a text's diagnostics.
///
/// Fails when `check` fails, such as on a syntax error, in the first round;
/// a later round's failure keeps the text as the previous round left it
/// and reports nothing remaining for it.
pub fn fix<E>(
    source: &str,
    mut check: impl FnMut(&str) -> Result<Vec<Diagnostic>, E>,
) -> Result<Fixed, E> {
    let mut text = source.to_owned();
    let mut applied = Vec::new();
    let mut seen = HashSet::new();
    let mut diagnostics = check(&text)?;
    for _ in 0..ROUNDS {
        seen.insert(text.clone());
        let chosen = choose(&diagnostics);
        if chosen.is_empty() {
            break;
        }
        let edits = chosen
            .iter()
            .flat_map(|(_, fix)| fix.edits.iter().cloned())
            .collect();
        let Some(next) = Fix::edits("", edits).apply(&text) else {
            break;
        };
        if seen.contains(&next) {
            break;
        }
        let mut repaired: Vec<&Diagnostic> =
            chosen.iter().map(|(diagnostic, _)| *diagnostic).collect();
        repaired.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
        for diagnostic in repaired {
            let position = diagnostic.span.position(&text);
            applied.push(Applied {
                diagnostic: diagnostic.clone(),
                line: position.line,
                column: position.column,
            });
        }
        text = next;
        match check(&text) {
            Ok(next) => diagnostics = next,
            Err(_) => {
                diagnostics = Vec::new();
                break;
            }
        }
    }
    Ok(Fixed {
        source: text,
        applied,
        remaining: diagnostics,
    })
}

/// The diagnostics whose machine-applicable fixes apply together this
/// round: innermost first, skipping any fix whose edits overlap or touch
/// one already chosen.
fn choose(diagnostics: &[Diagnostic]) -> Vec<(&Diagnostic, &Fix)> {
    let mut candidates: Vec<(&Diagnostic, &Fix)> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.file.is_none())
        .filter_map(|diagnostic| {
            let fix = diagnostic.applicable_fix()?;
            debug_assert_eq!(fix.applicability, Applicability::Always);
            (!fix.edits.is_empty()).then_some((diagnostic, fix))
        })
        .collect();
    candidates.sort_by_key(|(diagnostic, fix)| {
        let start = fix.edits.iter().map(|edit| edit.span.start).min();
        let end = fix.edits.iter().map(|edit| edit.span.end).max();
        let extent = end.unwrap_or(0) - start.unwrap_or(0);
        (extent, diagnostic.span.start)
    });
    let mut taken: Vec<Span> = Vec::new();
    let mut chosen = Vec::new();
    for (diagnostic, fix) in candidates {
        let clear = fix
            .edits
            .iter()
            .all(|edit| taken.iter().all(|span| !touch(edit.span, *span)));
        if clear {
            taken.extend(fix.edits.iter().map(|edit| edit.span));
            chosen.push((diagnostic, fix));
        }
    }
    chosen
}

/// Whether two edit spans overlap or meet, where the order of their text
/// would be ambiguous.
fn touch(a: Span, b: Span) -> bool {
    a.start <= b.end && b.start <= a.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibescript::diagnostic::Code;

    fn surface(text: &str) -> Result<Vec<Diagnostic>, vibescript::Error> {
        vibescript::Engine::new()
            .type_check(text)
            .map(|checked| checked.diagnostics)
    }

    #[test]
    fn nested_fixes_apply_over_rounds_innermost_first() {
        let source = "def check(x: int?)\n  puts 1 unless x.nil?\nend\n";
        let fixed = fix(source, surface).unwrap();
        assert_eq!(
            fixed.source,
            "def check(x: int?)\n  puts 1 if x != nil\nend\n"
        );
        let codes: Vec<Code> = fixed.applied.iter().map(|a| a.diagnostic.code).collect();
        assert_eq!(codes, [Code::NIL_PREDICATE, Code::UNLESS]);
        assert!(fixed.remaining.is_empty());
    }

    #[test]
    fn fixing_is_idempotent() {
        let source = "names = %w[a b]\nn = names.size()\nputs n unless n == 0\n";
        let once = fix(source, surface).unwrap();
        let twice = fix(&once.source, surface).unwrap();
        assert_eq!(twice.source, once.source);
        assert!(!twice.changed());
        assert_eq!(twice.remaining, once.remaining);
    }

    #[test]
    fn suggestions_and_manual_rewrites_remain() {
        let source = "a = 1\nok = a.eql?(1)\nb = [1].reduce(:+)\n";
        let fixed = fix(source, surface).unwrap();
        assert_eq!(fixed.source, source);
        let codes: Vec<Code> = fixed.remaining.iter().map(|d| d.code).collect();
        assert_eq!(codes, [Code::IDENTITY_EQUALITY, Code::REMOVED_NAME]);
    }

    #[test]
    fn a_syntax_error_fails() {
        assert!(fix("def (\n", surface).is_err());
    }

    #[test]
    fn touching_edits_wait_for_the_next_round() {
        assert!(touch(Span::new(0, 3), Span::new(3, 5)));
        assert!(touch(Span::at(2), Span::new(2, 4)));
        assert!(!touch(Span::new(0, 2), Span::new(3, 5)));
    }
}
