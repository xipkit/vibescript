//! Helpers that check a source and describe its diagnostics.

use vibescript::{
    Engine,
    diagnostic::{Applicability, Diagnostic},
};

/// The error diagnostics of `source`, checked by a plain engine.
pub fn errors(source: &str) -> Vec<Diagnostic> {
    errors_with(&Engine::new(), source)
}

/// The error diagnostics of `source`, checked by `engine`.
pub fn errors_with(engine: &Engine, source: &str) -> Vec<Diagnostic> {
    engine
        .type_check(source)
        .unwrap_or_else(|error| panic!("{source}\ndoes not parse: {error}"))
        .diagnostics
        .into_iter()
        .filter(Diagnostic::is_error)
        .collect()
}

/// Describes diagnostics for assertion messages.
pub fn describe(source: &str, diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| d.render(source))
        .collect::<Vec<_>>()
        .join("")
}

/// Asserts that `source` has no type errors.
#[track_caller]
pub fn clean(source: &str) {
    let found = errors(source);
    assert!(
        found.is_empty(),
        "expected no errors in\n{source}\nfound:\n{}",
        describe(source, &found)
    );
}

/// Asserts that `source` has exactly the errors with these codes, in
/// source order, and returns them.
#[track_caller]
pub fn codes(source: &str, expected: &[&str]) -> Vec<Diagnostic> {
    let found = errors(source);
    let actual: Vec<String> = found.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(
        actual,
        expected,
        "in\n{source}\nfound:\n{}",
        describe(source, &found)
    );
    found
}

/// Asserts that `source` has one error, with `code`, whose message contains
/// `text`, and returns it.
#[track_caller]
pub fn error(source: &str, code: &str, text: &str) -> Diagnostic {
    let found = codes(source, &[code]);
    let diagnostic = found.into_iter().next().unwrap();
    assert!(
        diagnostic.message.contains(text),
        "message {:?} lacks {text:?}",
        diagnostic.message
    );
    diagnostic
}

/// The text a diagnostic's primary span covers.
pub fn spanned<'a>(source: &'a str, diagnostic: &Diagnostic) -> &'a str {
    &source[diagnostic.span.start..diagnostic.span.end]
}

/// Applies a diagnostic's machine-applicable fix.
#[track_caller]
pub fn fixed(source: &str, diagnostic: &Diagnostic) -> String {
    let fix = diagnostic
        .fixes
        .iter()
        .find(|fix| fix.applicability == Applicability::Always)
        .unwrap_or_else(|| panic!("no fix on {diagnostic:?}"));
    fix.apply(source).expect("the fix applies")
}
