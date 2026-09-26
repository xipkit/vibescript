//! Compile diagnostics with stable codes and machine-applicable fixes (ADR-008).
//!
//! Every compile-time finding, such as a type error from the static checker
//! or a removed spelling, is a [`Diagnostic`]: a stable [`Code`], a
//! [`Severity`], a primary [`Span`] with optional labelled secondary spans, a
//! message, the expected and found types where they apply, and [`Fix`]es
//! made of text edits. A fix is [`Applicability::Always`] only when the repair
//! is unambiguous; a diagnostic with several plausible repairs offers none
//! rather than guessing.
//!
//! Codes are grouped by area, one hundred per area, and keep their meaning
//! across releases while the message text may improve. [`codes`] lists every
//! registered code with a one-line description.
//!
//! ```
//! use vibescript::diagnostic::{Code, Diagnostic, Fix, Span};
//! let source = "items = [1]\nn = items.size\n";
//! let diagnostic = Diagnostic::error(Code::REMOVED_NAME, Span::new(22, 26), "`size` was removed")
//!     .with_fix(Fix::replace("use `length`", Span::new(22, 26), "length"));
//! assert_eq!(diagnostic.code.to_string(), "V0401");
//! assert_eq!(diagnostic.fixes[0].apply(source).as_deref(), Some("items = [1]\nn = items.length\n"));
//! assert!(diagnostic.render(source).starts_with("error[V0401]: `size` was removed\n  --> 2:11\n"));
//! ```

use std::{fmt, sync::Arc};

/// A stable diagnostic code such as `V0101`.
///
/// The hundreds digit group names the [`Area`]. A code is never reused for a
/// different meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code(u16);

/// The area a [`Code`] belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Area {
    /// `V00xx`: source that does not parse.
    Syntax,
    /// `V01xx`: static types, including declarations, conditions and operators.
    Types,
    /// `V02xx`: names, members, instance variables and definite assignment.
    Names,
    /// `V03xx`: calls, overloads, keywords, blocks and `require`.
    Calls,
    /// `V04xx`: removed spellings and syntax of the canonical surface.
    Surface,
}

/// A registered code with its short name and one-line description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeInfo {
    pub code: Code,
    /// A kebab-case name, stable like the code, such as `type-mismatch`.
    pub name: &'static str,
    pub description: &'static str,
    /// Whether the code is retired: no diagnostic reports it any more, and
    /// its number is never given another meaning.
    pub retired: bool,
}

macro_rules! registry {
    (
        $($constant:ident = $number:literal, $name:literal, $description:literal;)*
        @retired {
            $($old:literal, $old_name:literal, $old_description:literal;)*
        }
    ) => {
        impl Code {
            $(
                #[doc = $description]
                pub const $constant: Code = Code($number);
            )*
        }

        const REGISTRY: &[CodeInfo] = &{
            let mut registry = [
                $(CodeInfo { code: Code($number), name: $name, description: $description, retired: false },)*
                $(CodeInfo { code: Code($old), name: $old_name, description: $old_description, retired: true },)*
            ];
            // Numeric order, with the retired codes among the others.
            let mut sorted = 1;
            while sorted < registry.len() {
                let mut at = sorted;
                while at > 0 && registry[at - 1].code.0 > registry[at].code.0 {
                    let previous = registry[at - 1];
                    registry[at - 1] = registry[at];
                    registry[at] = previous;
                    at -= 1;
                }
                sorted += 1;
            }
            registry
        };
    };
}

registry! {
    SYNTAX = 1, "syntax", "The source does not parse.";
    HASH_ARGUMENT = 2, "hash-argument", "A hash literal is passed to a call without parentheses, where `{` after the call starts a block; the call's arguments need parentheses.";

    TYPE_MISMATCH = 101, "type-mismatch", "A value's type is not assignable to the type its position expects.";
    LOCAL_TYPE_CHANGED = 102, "local-type-changed", "A local is assigned a value of a type other than the one its first assignment fixed.";
    NEEDS_TYPE = 103, "needs-type", "`nil`, `[]` or `{}` appears where no declared type gives it one.";
    CONDITION_NOT_BOOL = 104, "condition-not-bool", "A condition is not a `bool`.";
    LOGICAL_NOT_BOOL = 105, "logical-not-bool", "An operand of `!`, `&&` or `||` is not a `bool`.";
    ANY_USE = 106, "any-use", "A value of type `any` is used before it is narrowed.";
    OPTIONAL_USE = 107, "optional-use", "A value that may be `nil` is used where `nil` is not accepted.";
    NO_OPERATOR = 108, "no-operator", "An operator is not defined for its operand types.";
    UNKNOWN_FIELD = 110, "unknown-field", "A shape is read or written at a key it does not declare.";
    DYNAMIC_KEY = 111, "dynamic-key", "A shape is indexed with a key known only at runtime.";
    NOT_INDEXABLE = 112, "not-indexable", "A value of this type cannot be indexed this way.";
    TUPLE_INDEX = 113, "tuple-index", "A tuple is indexed outside its elements.";
    NON_EXHAUSTIVE_CASE = 114, "non-exhaustive-case", "A `case` over an enum or `bool` neither names every value nor has an `else`.";
    BOUND = 115, "bound", "A generic member's bound is not met, such as `sort` on a union element type.";
    UNKNOWN_TYPE = 116, "unknown-type", "An annotation names a type that does not exist.";
    RETURN_WITHOUT_TYPE = 117, "return-without-type", "A function without `-> T` returns a value.";
    MISSING_PARAMETER_TYPE = 118, "missing-parameter-type", "A parameter does not declare its type.";
    YIELD_VALUE = 119, "yield-value", "The value of `yield` is used, but the block declares no result type.";
    CAST = 120, "cast", "A checked cast or `is_type?` can never succeed for the value's type.";
    UNREACHABLE_NARROWING = 121, "unreachable-narrowing", "A nil test or type test on a value whose type already decides it.";
    TUPLE_MUTATION = 122, "tuple-mutation", "A mutation could change a tuple's length or positional element types.";

    UNDEFINED_NAME = 201, "undefined-name", "A name does not refer to a local, function, constant or type in scope.";
    UNASSIGNED_LOCAL = 202, "unassigned-local", "A local is read where it is not assigned on every path.";
    UNKNOWN_MEMBER = 203, "unknown-member", "A type has no member with this name.";
    UNDECLARED_IVAR = 204, "undeclared-ivar", "An instance variable is read or assigned without a declaration.";
    UNINITIALIZED_IVAR = 205, "uninitialized-ivar", "An instance variable without a default is not assigned on every path through `initialize`.";
    UNKNOWN_ENUM_MEMBER = 206, "unknown-enum-member", "A symbol or constant does not name a member of the enum.";
    BLOCK_NOT_VALUE = 207, "block-not-value", "A block parameter is used as a value.";
    VISIBILITY = 208, "visibility", "A private method is called with a receiver, or a protected one from outside its class's own methods.";
    DUPLICATE_NAME = 209, "duplicate-name", "A function, method or alias takes a name its scope already defines.";
    RESERVED_NAME = 210, "reserved-name", "A function or alias takes a reserved name: `require`, which the compiler resolves statically, or `__main__`.";

    NO_OVERLOAD = 301, "no-overload", "No signature accepts the call's positional arguments, keywords and block.";
    UNKNOWN_KEYWORD = 302, "unknown-keyword", "A call passes a keyword the signature does not declare.";
    MISSING_KEYWORD = 303, "missing-keyword", "A call omits a required keyword argument.";
    MISSING_BLOCK = 304, "missing-block", "A call omits a required block.";
    UNEXPECTED_BLOCK = 305, "unexpected-block", "A call passes a block to a function that takes none.";
    BLOCK_PARAMETERS = 306, "block-parameters", "A block declares parameters the signature does not provide.";
    UNGUARDED_YIELD = 307, "unguarded-yield", "A `yield` to an optional block is not guarded by `block_given?`.";
    UNDECLARED_BLOCK = 308, "undeclared-block", "A function yields but does not declare its block as a typed `&` parameter.";
    DYNAMIC_REQUIRE = 309, "dynamic-require", "A `require` names its module or alias with something other than a string literal.";
    NOT_CALLABLE = 310, "not-callable", "A value is called but is not a function.";

    INVALID_REQUIRE_ALIAS = 311, "invalid-require-alias", "A require alias is not a valid identifier or is a keyword.";

    REMOVED_NAME = 401, "removed-name", "A builtin member, function or namespace member is called by a removed name, such as `size` for `length`.";
    NIL_PREDICATE = 402, "nil-predicate", "`nil?` is removed; `x == nil` tests for nil.";
    IDENTITY_EQUALITY = 403, "identity-equality", "`eql?` and `equal?` are removed; `==` compares values.";
    IDENTITY_CALL = 404, "identity-call", "`itself`, `tap` and `yield_self` are removed; the expression itself replaces them.";
    DISPATCH_BY_NAME = 405, "dispatch-by-name", "`send`, `public_send` and `respond_to?` are removed; call the member directly.";
    DO_BLOCK = 406, "do-block", "A block is written `do ... end`; blocks are written with braces.";
    UNLESS = 407, "unless", "`unless` is removed; `if` takes the negated condition.";
    UNTIL = 408, "until", "`until` is removed; `while` takes the negated condition.";
    SYMBOL_KEY = 409, "symbol-key", "A hash is indexed with a symbol; hash keys are strings.";
    PERCENT_LITERAL = 410, "percent-literal", "A percent literal such as `%w[a b]`; arrays are written as array literals.";
    HASH_NEW = 411, "hash-new", "`Hash.new` is removed; `{}` with a declared type makes a hash.";
    EMPTY_PARENTHESES = 412, "empty-parentheses", "A call without arguments is written with `()`; it takes no parentheses.";
    TYPE_NAME = 413, "type-name", "A builtin type name is spelled other than in lowercase, or as `object` for `hash`.";
    KEYWORD_PARAMETER = 414, "keyword-parameter", "A keyword parameter is declared as `name:`, `name: default` or `name: T:`; keyword parameters follow a bare `*`.";
    FIELD_ACCESS = 415, "field-access", "A hash or shape field is read or written with a dot; dot calls methods, and a field is indexed as `h[\"name\"]`.";
    SCOPED_CALL = 416, "scoped-call", "A function or method is called with `::`, as in `JSON::parse(x)`; `::` names only constants, nested types and enum members, and a dot calls.";

    @retired {
        109, "integer-division", "Retired when `/` became true division: it rejected `/` on two ints, whose floor division `//` now spells.";
    }
}

impl Code {
    /// Creates a code from its number, such as `101` for `V0101`.
    pub const fn from_number(number: u16) -> Self {
        Self(number)
    }

    /// The code's number, such as `101` for `V0101`.
    pub const fn number(self) -> u16 {
        self.0
    }

    /// Parses a code written as `V0101`.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('V')?;
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().map(Self)
    }

    /// The area the code's hundreds group names, if it is a known area.
    pub fn area(self) -> Option<Area> {
        Some(match self.0 / 100 {
            0 => Area::Syntax,
            1 => Area::Types,
            2 => Area::Names,
            3 => Area::Calls,
            4 => Area::Surface,
            _ => return None,
        })
    }

    /// The registered name and description, or none for an unregistered code.
    pub fn info(self) -> Option<&'static CodeInfo> {
        REGISTRY.iter().find(|info| info.code == self)
    }

    /// The registered kebab-case name, or an empty string.
    pub fn name(self) -> &'static str {
        self.info().map_or("", |info| info.name)
    }

    /// The registered one-line description, or an empty string.
    pub fn description(self) -> &'static str {
        self.info().map_or("", |info| info.description)
    }

    /// Whether the code is registered as retired: no diagnostic reports it
    /// any more.
    pub fn retired(self) -> bool {
        self.info().is_some_and(|info| info.retired)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "V{:04}", self.0)
    }
}

/// Every registered code in numeric order, retired ones included.
pub fn codes() -> &'static [CodeInfo] {
    REGISTRY
}

/// How serious a [`Diagnostic`] is. Errors stop compilation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// `error` or `warning`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// A half-open byte range in a source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    /// A span from `start` to `end`; an `end` before `start` is clamped to it.
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    /// An empty span at `offset`, for insertions.
    pub fn at(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    /// The one-based line and Unicode character column of the span's start.
    pub fn position(self, source: &str) -> crate::Position {
        position(source, self.start)
    }
}

fn position(source: &str, offset: usize) -> crate::Position {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &source[..offset];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    crate::Position {
        line: before.bytes().filter(|&b| b == b'\n').count() + 1,
        column: before[line_start..].chars().count() + 1,
    }
}

/// A secondary span with a note, such as "declared here".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
    /// The root-relative filename of a required module the span is in, or
    /// none when it is in the same source as the diagnostic.
    pub file: Option<Arc<[u8]>>,
}

/// Replaces the text a span covers; an empty span inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

/// Whether a tool may apply a [`Fix`] without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The repair is unambiguous; `vibes fix` and code actions apply it.
    Always,
    /// A plausible repair that a person should confirm.
    Suggestion,
}

/// A set of edits that is valid on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// What the fix does, such as "declare `counts: hash<string, int>`".
    pub message: String,
    /// Non-overlapping edits in the diagnostic's source.
    pub edits: Vec<Edit>,
    pub applicability: Applicability,
}

impl Fix {
    /// A machine-applicable fix that replaces one span.
    pub fn replace(message: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        Self::edits(
            message,
            vec![Edit {
                span,
                replacement: replacement.into(),
            }],
        )
    }

    /// A machine-applicable fix that inserts text at `offset`.
    pub fn insert(message: impl Into<String>, offset: usize, text: impl Into<String>) -> Self {
        Self::replace(message, Span::at(offset), text)
    }

    /// A machine-applicable fix made of several edits.
    pub fn edits(message: impl Into<String>, edits: Vec<Edit>) -> Self {
        Self {
            message: message.into(),
            edits,
            applicability: Applicability::Always,
        }
    }

    /// Marks the fix as a suggestion a person should confirm.
    pub fn suggestion(mut self) -> Self {
        self.applicability = Applicability::Suggestion;
        self
    }

    /// Applies the edits to `source`, or returns none when an edit is out of
    /// range, splits a character, or overlaps another.
    pub fn apply(&self, source: &str) -> Option<String> {
        let mut edits: Vec<&Edit> = self.edits.iter().collect();
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        let mut output = String::with_capacity(source.len());
        let mut cursor = 0;
        for edit in edits {
            let Span { start, end } = edit.span;
            if start < cursor
                || end > source.len()
                || !source.is_char_boundary(start)
                || !source.is_char_boundary(end)
            {
                return None;
            }
            output.push_str(&source[cursor..start]);
            output.push_str(&edit.replacement);
            cursor = end;
        }
        output.push_str(&source[cursor..]);
        Some(output)
    }
}

/// One compile-time finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    /// The root-relative filename of the required module the diagnostic is
    /// in, or none for the compiled source itself.
    pub file: Option<Arc<[u8]>>,
    /// The required module's source, when the caller's source is a different file.
    pub source: Option<Arc<str>>,
    /// Where the problem is.
    pub span: Span,
    pub message: String,
    /// Related places, such as the declaration a value is checked against.
    pub labels: Vec<Label>,
    /// The type the position expects, as written in annotations.
    pub expected: Option<String>,
    /// The type the value has, as written in annotations.
    pub found: Option<String>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    /// An error with no labels, types or fixes.
    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            file: None,
            source: None,
            span,
            message: message.into(),
            labels: Vec::new(),
            expected: None,
            found: None,
            fixes: Vec::new(),
        }
    }

    /// A warning with no labels, types or fixes.
    pub fn warning(code: Code, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, span, message)
        }
    }

    /// Adds a labelled secondary span in the same source.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
            file: None,
        });
        self
    }

    /// Records the expected and found types.
    pub fn with_types(mut self, expected: impl Into<String>, found: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self.found = Some(found.into());
        self
    }

    /// Adds a fix.
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }

    /// Places the diagnostic in a required module's file.
    pub fn in_file(mut self, file: Option<Arc<[u8]>>) -> Self {
        self.file = file;
        self
    }

    /// Whether the diagnostic stops compilation.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The first fix that tools may apply without asking.
    pub fn applicable_fix(&self) -> Option<&Fix> {
        self.fixes
            .iter()
            .find(|fix| fix.applicability == Applicability::Always)
    }

    /// Renders the diagnostic for a terminal, with the source line it points
    /// at, its labels, types and fixes. `source` is the text the spans index.
    pub fn render(&self, source: &str) -> String {
        let source = self.source.as_deref().unwrap_or(source);
        let mut out = format!(
            "{}[{}]: {}\n",
            self.severity.as_str(),
            self.code,
            self.message
        );
        let crate::Position { line, column } = self.span.position(source);
        let file = self
            .file
            .as_deref()
            .map(|name| format!("{}:", String::from_utf8_lossy(name)))
            .unwrap_or_default();
        out.push_str(&format!("  --> {file}{line}:{column}\n"));
        if let Some(text) = source.lines().nth(line - 1) {
            let width = self.span.end.min(line_end(source, self.span.start)) - self.span.start;
            let marked = source
                .get(self.span.start..self.span.start + width)
                .map_or(1, |text| text.chars().count().max(1));
            out.push_str(&format!(
                "   |\n{line:>3}| {text}\n   | {}{}\n",
                " ".repeat(column - 1),
                "^".repeat(marked)
            ));
        }
        if let (Some(expected), Some(found)) = (&self.expected, &self.found) {
            out.push_str(&format!("   = expected {expected}, found {found}\n"));
        }
        for label in &self.labels {
            let position = label.span.position(source);
            let place = match &label.file {
                Some(name) => format!("{}:", String::from_utf8_lossy(name)),
                None => String::new(),
            };
            out.push_str(&format!(
                "   = note: {} ({place}{}:{})\n",
                label.message, position.line, position.column
            ));
        }
        for fix in &self.fixes {
            out.push_str(&format!("   = fix: {}\n", fix.message));
        }
        out
    }

    /// One JSON object describing the diagnostic, as `vibes check --json`
    /// prints it. Positions are one-based lines and character columns in
    /// `source`; byte offsets are included as `start` and `end`.
    pub fn to_json(&self, source: &str) -> String {
        let source = self.source.as_deref().unwrap_or(source);
        let span = |span: Span| {
            let start = span.position(source);
            let end = position(source, span.end);
            format!(
                "{{\"start\":{},\"end\":{},\"line\":{},\"column\":{},\"end_line\":{},\"end_column\":{}}}",
                span.start, span.end, start.line, start.column, end.line, end.column
            )
        };
        let file = |file: &Option<Arc<[u8]>>| match file {
            Some(name) => json_string(&String::from_utf8_lossy(name)),
            None => "null".to_owned(),
        };
        let optional =
            |text: &Option<String>| text.as_deref().map_or("null".to_owned(), json_string);
        let labels: Vec<String> = self
            .labels
            .iter()
            .map(|label| {
                format!(
                    "{{\"span\":{},\"message\":{},\"file\":{}}}",
                    span(label.span),
                    json_string(&label.message),
                    file(&label.file)
                )
            })
            .collect();
        let fixes: Vec<String> = self
            .fixes
            .iter()
            .map(|fix| {
                let edits: Vec<String> = fix
                    .edits
                    .iter()
                    .map(|edit| {
                        format!(
                            "{{\"span\":{},\"replacement\":{}}}",
                            span(edit.span),
                            json_string(&edit.replacement)
                        )
                    })
                    .collect();
                format!(
                    "{{\"message\":{},\"applicability\":{},\"edits\":[{}]}}",
                    json_string(&fix.message),
                    json_string(match fix.applicability {
                        Applicability::Always => "always",
                        Applicability::Suggestion => "suggestion",
                    }),
                    edits.join(",")
                )
            })
            .collect();
        format!(
            "{{\"code\":\"{}\",\"name\":{},\"severity\":\"{}\",\"file\":{},\"span\":{},\"message\":{},\"expected\":{},\"found\":{},\"labels\":[{}],\"fixes\":[{}]}}",
            self.code,
            json_string(self.code.name()),
            self.severity.as_str(),
            file(&self.file),
            span(self.span),
            json_string(&self.message),
            optional(&self.expected),
            optional(&self.found),
            labels.join(","),
            fixes.join(",")
        )
    }
}

impl fmt::Display for Diagnostic {
    /// `error[V0101]: message`, without source context.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}]: {}",
            self.severity.as_str(),
            self.code,
            self.message
        )
    }
}

fn line_end(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    source[offset..]
        .find('\n')
        .map_or(source.len(), |index| offset + index)
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_ordered_and_grouped() {
        let codes = codes();
        for pair in codes.windows(2) {
            assert!(
                pair[0].code < pair[1].code,
                "{} {}",
                pair[0].code,
                pair[1].code
            );
        }
        let mut names: Vec<_> = codes.iter().map(|info| info.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), codes.len());
        for info in codes {
            assert!(info.code.area().is_some(), "{}", info.code);
            assert_eq!(Code::parse(&info.code.to_string()), Some(info.code));
            assert!(info.description.ends_with('.'), "{}", info.code);
        }
        assert_eq!(Code::parse("V101"), None);
        assert_eq!(Code::parse("X0101"), None);
        assert_eq!(Code::TYPE_MISMATCH.area(), Some(Area::Types));
        // `/` on two ints divides now, so V0109 stays registered, retired.
        let retired: Vec<_> = codes.iter().filter(|info| info.retired).collect();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].code, Code::from_number(109));
        assert!(Code::from_number(109).retired());
        assert!(!Code::TYPE_MISMATCH.retired());
    }

    #[test]
    fn fixes_apply_in_order_and_refuse_overlaps() {
        let fix = Fix::edits(
            "two edits",
            vec![
                Edit {
                    span: Span::new(4, 5),
                    replacement: "b".into(),
                },
                Edit {
                    span: Span::at(0),
                    replacement: "x: ".into(),
                },
            ],
        );
        assert_eq!(fix.apply("a = a").as_deref(), Some("x: a = b"));
        let overlapping = Fix::edits(
            "overlap",
            vec![
                Edit {
                    span: Span::new(0, 3),
                    replacement: String::new(),
                },
                Edit {
                    span: Span::new(2, 4),
                    replacement: String::new(),
                },
            ],
        );
        assert_eq!(overlapping.apply("abcdef"), None);
        assert_eq!(Fix::replace("out", Span::new(3, 9), "").apply("abc"), None);
    }

    #[test]
    fn json_escapes_and_locates_spans() {
        let source = "x = 1\ny = \"é\" + 1\n";
        let diagnostic = Diagnostic::error(
            Code::NO_OPERATOR,
            Span::new(10, 18),
            "no `+` for \"string\"",
        )
        .with_types("string", "int")
        .with_label(Span::new(0, 1), "declared here");
        let json = diagnostic.to_json(source);
        assert!(json.starts_with(
            "{\"code\":\"V0108\",\"name\":\"no-operator\",\"severity\":\"error\",\"file\":null,"
        ));
        assert!(json.contains("\"span\":{\"start\":10,\"end\":18,\"line\":2,\"column\":5,\"end_line\":2,\"end_column\":12}"));
        assert!(json.contains("\"message\":\"no `+` for \\\"string\\\"\""));
        assert!(json.contains("\"expected\":\"string\",\"found\":\"int\""));
        let rendered = diagnostic.render(source);
        assert!(rendered.contains("  --> 2:5\n"), "{rendered}");
        assert!(
            rendered.contains("  2| y = \"é\" + 1\n   |     ^^^^^^^\n"),
            "{rendered}"
        );
    }
}
