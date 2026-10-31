use super::*;

/// Reports the `?` or `!` at `offset` in `source`, one of a name's run of
/// them. A `method` name keeps the run's last character unless a setter's
/// `=` follows; any other name loses the whole run. A repair is offered only
/// when what remains is a valid name: not for `@?`, whose name would be
/// empty, nor for `x?1`, where the name continues and removing the `?` could
/// name something else.
pub(crate) fn name_suffix_error(
    work: &dyn Work,
    source: &str,
    offset: usize,
    method: bool,
) -> Error {
    use crate::diagnostic::{Code, Diagnostic, Fix, Span};
    let span = Span::new(offset, offset + 1);
    let internal = "`?` and `!` may only end a method name";
    let message = "only method names may end in `?` or `!`; remove the suffix from this name";
    let diagnostic = match suffix_repair(source, offset, method) {
        Repair::Internal => {
            let diagnostic = Diagnostic::error(Code::NAME_SUFFIX, span, internal);
            return Error::syntax(work, offset, internal).with_diagnostic(diagnostic);
        }
        Repair::None => Diagnostic::error(Code::NAME_SUFFIX, span, message),
        Repair::Edit(label, edit, replacement) => Diagnostic::error(
            Code::NAME_SUFFIX,
            span,
            message,
        )
        .with_fix(Fix::replace(label, edit, replacement)),
    };
    Error::syntax(work, offset, message).with_diagnostic(diagnostic)
}

/// How a name's suffix is repaired.
enum Repair {
    /// The name continues after the `?` or `!`, which is no suffix.
    Internal,
    /// No valid name remains without the suffix.
    None,
    /// Replaces the span, with a label.
    Edit(&'static str, crate::diagnostic::Span, &'static str),
}

/// The repair of the run of `?` and `!` around `offset`: its removal, or an
/// underscore where the name would become a keyword that cannot stand there.
fn suffix_repair(source: &str, offset: usize, method: bool) -> Repair {
    let bytes = source.as_bytes();
    let suffix = |at: usize| matches!(bytes.get(at), Some(b'?' | b'!'));
    let mut run = offset;
    while run > 0 && suffix(run - 1) {
        run -= 1;
    }
    let mut end = offset + 1;
    while suffix(end) {
        end += 1;
    }
    if source[end..].starts_with(|c: char| c == '_' || unicode::letter_or_digit(c)) {
        return Repair::Internal;
    }
    let start = source[..run]
        .rfind(|c: char| c != '_' && !unicode::letter_or_digit(c))
        .map_or(0, |i| i + source[i..].chars().next().unwrap().len_utf8());
    let stem = &source[start..run];
    if !identifier(stem) {
        return Repair::None;
    }
    // A method keeps one suffix; a setter and every binding keep none.
    if method && end - run > 1 && bytes.get(end) != Some(&b'=') {
        let span = crate::diagnostic::Span::new(run, end - 1);
        return Repair::Edit(suffixes::REPEATED, span, "");
    }
    let span = crate::diagnostic::Span::new(run, end);
    if keyword(stem) && !keyword_allowed(&source[..start]) {
        Repair::Edit("replace the name suffix with an underscore", span, "_")
    } else {
        Repair::Edit("remove the name suffix", span, "")
    }
}

/// Whether a keyword can be the name that follows `before`: a variable's
/// after its sigil, a symbol's, or a member's, except in `def self.name`.
fn keyword_allowed(before: &str) -> bool {
    let before = before.trim_end();
    if before.ends_with(['@', ':']) {
        return true;
    }
    let Some(receiver) = before.strip_suffix('.').map(str::trim_end) else {
        return false;
    };
    !receiver
        .strip_suffix("self")
        .is_some_and(|rest| rest.trim_end().ends_with("def"))
}

/// The registration a host-supplied name comes from, as its errors name it.
/// Such a name has no position in any script, so its errors carry no fix.
#[derive(Clone, Copy)]
pub(crate) struct HostName<'a> {
    kind: &'a str,
    owner: Option<(&'a str, &'a str)>,
    value: bool,
}

impl<'a> HostName<'a> {
    /// A function registered on the engine.
    pub(crate) const FUNCTION: Self = Self::new("host function");
    /// A global declared on the engine or supplied by a call.
    pub(crate) const GLOBAL: Self = Self::new("global");
    /// A capability's root name.
    pub(crate) const CAPABILITY: Self = Self::new("capability");
    /// A method in a host object that reaches a script.
    pub(crate) const METHOD: Self = Self::new("method");

    const fn new(kind: &'a str) -> Self {
        Self {
            kind,
            owner: None,
            value: false,
        }
    }

    /// A method in the value this registration binds to `name`.
    pub(crate) fn member_of(self, name: &'a str) -> Self {
        Self {
            owner: Some((self.kind, name)),
            ..Self::METHOD
        }
    }

    /// The same registration, known to be bound to the value being checked.
    pub(crate) fn bound(self) -> Self {
        Self {
            value: true,
            ..self
        }
    }

    /// The error for a root bound to a function a script exports, which no
    /// script can call as a root.
    pub(crate) fn exported_root(self, name: &str) -> Error {
        // Enough bytes for `source_text` to mark a cut.
        let shown = String::from_utf8_lossy(&name.as_bytes()[..name.len().min(72)]);
        Error::new(
            crate::ErrorKind::Argument,
            format!(
                "{} \"{}\" is bound to a function a script exports, which a script calls only \
                 through its module: register a host function instead",
                self.kind,
                source_text(&shown)
            ),
        )
    }

    /// The error for a template, named by this method's owner, that holds a
    /// function a script exports at `key`.
    pub(crate) fn exported_member(self, key: &[u8]) -> Error {
        let shown = String::from_utf8_lossy(&key[..key.len().min(72)]);
        let (kind, owner) = self.owner.unwrap_or((self.kind, ""));
        Error::new(
            crate::ErrorKind::Argument,
            format!(
                "{kind} \"{}\" holds a function a script exports at \"{}\", which a script \
                 calls only through its module: register a host function instead",
                source_text(owner),
                source_text(&shown)
            ),
        )
    }

    fn error(self, name: &[u8], reason: impl std::fmt::Display) -> Error {
        use std::fmt::Write;
        // Enough bytes for `source_text` to mark a cut.
        let shown = String::from_utf8_lossy(&name[..name.len().min(72)]);
        let mut message = format!("invalid {} name \"{}\"", self.kind, source_text(&shown));
        if let Some((kind, owner)) = self.owner {
            let _ = write!(message, " in {kind} \"{}\"", source_text(owner));
        }
        let _ = write!(message, ": {reason}");
        Error::new(crate::ErrorKind::Argument, message)
    }
}

/// Checks a host binding against the same suffix rule as source bindings.
pub(crate) fn binding_name(work: &dyn Work, host: HostName<'_>, name: &str) -> Result<()> {
    work.checkpoint()?;
    work.bytes(name.len())?;
    if name_suffix_position(name).is_some() {
        let reason = if host.value {
            "only method names may end in `?` or `!`, and this value is not callable"
        } else {
            "only method names may end in `?` or `!`"
        };
        return Err(host.error(name.as_bytes(), reason));
    }
    // A script could never read another spelling, nor the prelude declare it.
    if !identifier(name) {
        return Err(host.error(
            name.as_bytes(),
            "a name starts with a letter or `_` and continues with letters, digits and `_`",
        ));
    }
    if keyword(name) {
        return Err(host.error(name.as_bytes(), "a keyword cannot name a binding"));
    }
    Ok(())
}

/// Whether `name` is spelled as a local's name: a letter or `_`, then
/// letters, digits and `_`.
pub(crate) fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || unicode::letter(c))
        && chars.all(|c| c == '_' || unicode::letter_or_digit(c))
}

pub(super) fn name_suffix_position(name: &str) -> Option<usize> {
    memchr::memchr2(b'?', b'!', name.as_bytes())
}

/// The operators an instance answers with the method of the same name: every
/// binary operator that is not short-circuiting, `<<`, and indexing. Each is
/// paired with whether `def` can spell it; an alias may name any of them.
const OPERATOR_METHODS: [(&str, bool); 21] = [
    ("+", true),
    ("-", true),
    ("*", true),
    ("/", true),
    ("//", false),
    ("%", true),
    ("**", true),
    ("<<", true),
    ("&", true),
    ("==", true),
    ("!=", true),
    ("===", false),
    ("=~", false),
    ("!~", false),
    ("<", true),
    ("<=", true),
    (">", true),
    (">=", true),
    ("<=>", true),
    ("[]", true),
    ("[]=", true),
];

/// Operators a symbol can spell that never call a method: `!`, `&&` and `||`
/// act on bools, and `|` is no binary operator.
const UNDISPATCHED_OPERATORS: [&str; 4] = ["!", "&&", "||", "|"];

/// Whether `def` can define the operator method `op`.
pub(super) fn def_operator(op: &str) -> bool {
    OPERATOR_METHODS.contains(&(op, true))
}

/// A method spelling rejected before a callable can be published.
enum MethodNameError {
    Suffix(usize),
    Undispatched(&'static str),
    Invalid,
}

impl MethodNameError {
    pub(crate) fn diagnostic(&self, work: &dyn Work, source: &str, offset: usize) -> Error {
        self.diagnostic_as(work, source, offset, true)
    }

    /// The error for a name at `offset` that is a `method`'s, which keeps one
    /// suffix, or a binding's, which keeps none.
    fn diagnostic_as(&self, work: &dyn Work, source: &str, offset: usize, method: bool) -> Error {
        match self {
            Self::Suffix(suffix) => name_suffix_error(work, source, offset + suffix, method),
            _ => Error::syntax(work, offset, self.message()),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Undispatched(op) => {
                format!(
                    "`{op}` cannot name a method: `!`, `&&`, `||` and `|` never dispatch to methods"
                )
            }
            _ => "invalid method name".to_owned(),
        }
    }

    /// Why a host's method name is invalid, with no source to repair.
    fn reason(&self) -> String {
        match self {
            Self::Suffix(_) => {
                "a method name may end in one `?` or `!`, and has neither elsewhere".to_owned()
            }
            Self::Undispatched(_) => self.message(),
            Self::Invalid => HOST_METHOD_SPELLING.to_owned(),
        }
    }
}

const HOST_METHOD_SPELLING: &str = "a method name starts with a letter or `_`, continues with letters, digits and `_`, and may end in one `?`, `!` or `=`";

/// Validates callable spellings, including operator methods and setters.
/// Lexer identifiers already satisfy the character rule; host names and decoded
/// symbols need that check too.
fn method_spelling(name: &str, lexed: bool) -> std::result::Result<(), MethodNameError> {
    if OPERATOR_METHODS.iter().any(|(op, _)| *op == name) {
        return Ok(());
    }
    if let Some(op) = UNDISPATCHED_OPERATORS.iter().find(|op| **op == name) {
        return Err(MethodNameError::Undispatched(op));
    }
    let stem = if let Some(suffix) = name_suffix_position(name) {
        if suffix + 1 != name.len() || name.starts_with('@') {
            return Err(MethodNameError::Suffix(suffix));
        }
        &name[..suffix]
    } else {
        name.strip_suffix('=').unwrap_or(name)
    };
    if lexed || identifier(stem) {
        return Ok(());
    }
    Err(MethodNameError::Invalid)
}

/// Validates a host function that scripts must be able to call without a receiver.
pub(crate) fn host_function_name(work: &dyn Work, host: HostName<'_>, name: &str) -> Result<()> {
    host_method_name(work, host, name.as_bytes(), true)?;
    if keyword(name) {
        return Err(host.error(name.as_bytes(), "a keyword cannot name a host function"));
    }
    if name.ends_with('=') {
        return Err(host.error(
            name.as_bytes(),
            "a setter is called through a receiver, so it cannot be a host function",
        ));
    }
    Ok(())
}

/// Checks a method's published name; a host method's diagnostic label is
/// separate. A script's function may be a setter such as `value=`, as a
/// module's `def value=` is, where a `setter` is allowed; a host method may
/// not, since no script can call a host setter.
pub(crate) fn host_method_name(
    work: &dyn Work,
    host: HostName<'_>,
    key: &[u8],
    setter: bool,
) -> Result<()> {
    work.checkpoint()?;
    work.bytes(key.len())?;
    let Ok(name) = std::str::from_utf8(key) else {
        return Err(host.error(key, "method names must be UTF-8"));
    };
    method_spelling(name, false).map_err(|error| host.error(key, error.reason()))?;
    if !name.starts_with(|c: char| c == '_' || unicode::letter(c)) {
        return Err(host.error(key, HOST_METHOD_SPELLING));
    }
    if !setter && name.ends_with('=') {
        return Err(host.error(
            key,
            "scripts cannot call a host method as a setter; publish one such as `set_value` instead",
        ));
    }
    Ok(())
}

impl Parser<'_> {
    /// Reports whether the token at `index` is an identifier, as opposed to a
    /// keyword or variable.
    pub(super) fn ident(&self, index: usize) -> bool {
        matches!(&self.tokens[index].token, Token::Word(w) if !keyword(w) && !w.starts_with('@'))
    }
    /// Consumes a symbol token and returns its name.
    pub(super) fn symbol_name(&mut self) -> Result<Option<Name>> {
        let name = match self.token() {
            Token::Symbol(name) => Name::new(self.work, name)?,
            Token::QuotedSymbol(bytes) => {
                self.work.bytes(bytes.len())?;
                let name = std::str::from_utf8(bytes)
                    .map_err(|_| unsupported(self.work, "method names must be UTF-8"))?;
                Name::new(self.work, name)?
            }
            _ => return Ok(None),
        };
        self.work.checkpoint()?;
        self.work.bytes(name.len())?;
        if let Err(error) = method_spelling(&name, false)
            && !suffixes::lenient()
        {
            let token = &self.tokens[self.pos];
            if matches!(token.token, Token::QuotedSymbol(_)) {
                use crate::diagnostic::{Code, Diagnostic, Fix, Span};
                let message = error.message();
                let span = Span::new(token.offset, token.end);
                let mut diagnostic = Diagnostic::error(Code::SYNTAX, span, &message);
                if let MethodNameError::Suffix(suffix) = error {
                    diagnostic = Diagnostic::error(Code::NAME_SUFFIX, span, &message);
                    // As in source, a `?` or `!` inside the name has no
                    // repair, and a repeated one keeps its last character.
                    if name[suffix..].bytes().all(|b| matches!(b, b'?' | b'!'))
                        && identifier(&name[..suffix])
                    {
                        let fixed =
                            Name::join(self.work, &[&name[..suffix], &name[name.len() - 1..]])?;
                        diagnostic = diagnostic.with_fix(Fix::replace(
                            "remove the name suffix",
                            span,
                            format!(":{:?}", fixed.as_str()),
                        ));
                    }
                }
                return Err(
                    Error::syntax(self.work, token.offset, message).with_diagnostic(diagnostic)
                );
            }
            return Err(error.diagnostic(self.work, self.source, token.offset + 1));
        }
        self.bump()?;
        Ok(Some(name))
    }
    pub(super) fn name(&mut self) -> Result<Name> {
        self.read_name(false)
    }
    pub(super) fn method_name(&mut self) -> Result<Name> {
        self.read_name(true)
    }
    fn read_name(&mut self, method: bool) -> Result<Name> {
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset;
        if let Token::Word(w) = self.bump()? {
            if method {
                self.method_spelling(&w, offset)?;
            } else {
                self.binding_name(&w, offset)?;
            }
            if reserved(&w) {
                return Err(Error::syntax(self.work, offset, "reserved name"));
            }
            Name::new(self.work, &w)
        } else {
            Err(Error::syntax(self.work, offset, "expected name"))
        }
    }
    pub(super) fn binding_name(&self, name: &str, offset: usize) -> Result<()> {
        if suffixes::lenient() {
            return Ok(());
        }
        if let Some(suffix) = name_suffix_position(name) {
            return Err(self.name_suffix_error(offset + suffix));
        }
        Ok(())
    }
    pub(super) fn method_spelling(&self, name: &str, offset: usize) -> Result<()> {
        self.work.checkpoint()?;
        self.work.bytes(name.len())?;
        if suffixes::lenient() {
            return Ok(());
        }
        method_spelling(name, true)
            .map_err(|error| error.diagnostic(self.work, self.source, offset))
    }
    /// Whether the name at `index`, with the receivers it is read through,
    /// such as `obj.name` or `@name`, starts a statement, where an assignment
    /// to it can stand, or follows `def`, where it names a setter.
    pub(super) fn assignment_starts(&self, index: usize) -> bool {
        let mut first = index;
        while let Some(previous) = first.checked_sub(1) {
            match &self.tokens[previous].token {
                Token::P('.') | Token::Op("&." | "::") => match previous.checked_sub(1) {
                    Some(receiver) if matches!(self.tokens[receiver].token, Token::Word(_)) => {
                        first = receiver;
                    }
                    _ => return false,
                },
                _ => break,
            }
        }
        match first.checked_sub(1) {
            None => true,
            Some(previous) => {
                matches!(
                    &self.tokens[previous].token,
                    Token::EndLine | Token::P('{' | '|')
                ) || matches!(&self.tokens[previous].token, Token::Word(word)
                if matches!(word.as_str(), "def" | "then" | "do" | "else" | "begin" | "ensure"))
            }
        }
    }
    /// Checks a name just read, bare or after a receiver, which names no
    /// method when it is assigned, listed first among destructuring targets
    /// or a `for` loop's variable.
    pub(super) fn read_spelling(&self, name: &str, offset: usize) -> Result<()> {
        self.work.checkpoint()?;
        self.work.bytes(name.len())?;
        if suffixes::lenient() {
            return Ok(());
        }
        // A name that is a whole destructuring target, even in a nested
        // group such as `a, [x, y] = ...`, binds.
        let target = self.target_start == Some(self.pos - 1)
            && matches!(self.tokens[self.pos].token, Token::P(',' | ']' | ')' | ':'));
        let binding = target
            || match &self.tokens[self.pos].token {
                // A comma after a name that does not start a statement separates
                // an element or argument, as in `{ a: s.empty??, b: 1 }`.
                Token::P(',') => self.assignment_starts(self.pos - 1),
                Token::Op(op) => assignment(op),
                // A `for` loop's variable.
                Token::Word(word) => word == "in",
                _ => false,
            };
        method_spelling(name, true)
            .map_err(|error| error.diagnostic_as(self.work, self.source, offset, !binding))
    }
    pub(super) fn name_suffix_error(&self, offset: usize) -> Error {
        name_suffix_error(self.work, self.source, offset, false)
    }
    pub(super) fn assignment_member(&self, expr: &Expr) -> Result<()> {
        if !suffixes::lenient()
            && matches!(&expr.node, Node::Member(_, name) | Node::SafeMember(_, name)
            if name.ends_with(['?', '!']))
        {
            // Member offsets point to the receiver; its name is the last word
            // before any closing parentheses around the target.
            for index in (0..self.pos).rev() {
                self.work.charge(1)?;
                let token = &self.tokens[index];
                if matches!(token.token, Token::Word(_)) {
                    return Err(self.name_suffix_error(token.end - 1));
                }
            }
        }
        Ok(())
    }
}

pub(super) fn reserved(w: &str) -> bool {
    matches!(
        w,
        "class"
            | "enum"
            | "for"
            | "in"
            | "when"
            | "until"
            | "begin"
            | "rescue"
            | "ensure"
            | "def"
            | "export"
            | "unless"
            | "case"
            | "yield"
            | "retry"
            | "raise"
            | "end"
            | "else"
            | "elsif"
            | "do"
            | "then"
            | "if"
            | "while"
            | "return"
            | "break"
            | "next"
    )
}

/// Every reserved word, sorted: the [`reserved`] words and the literal and
/// declaration words that cannot name a method or variable either.
pub(crate) const KEYWORDS: [&str; 34] = [
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "enum",
    "export", "false", "for", "getter", "if", "in", "next", "nil", "private", "property", "raise",
    "rescue", "retry", "return", "self", "setter", "then", "true", "unless", "until", "when",
    "while", "yield",
];

pub(crate) fn keyword(w: &str) -> bool {
    KEYWORDS.binary_search(&w).is_ok()
}
