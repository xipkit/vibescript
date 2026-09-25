//! What a walk's implementor knows about a program's types, and the rules
//! it adds beside the canonical surface's.

use super::{
    Finding,
    context::{Place, Surface},
    patterns::Pattern,
    syntax::*,
};
use std::ops::DerefMut;

/// Whether a condition's value is a `bool`, and how to make it one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Test {
    /// It is a `bool` already, or nothing says otherwise.
    Bool,
    /// It is `nil` or a value that is never `false`: test `!= nil`.
    Present,
    /// It is `nil`, `true` or `false`: test `== true`.
    True,
    /// Something else, or unknown, and why.
    Unknown(String),
}

/// Where a condition's types were observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Where the branch or operator at this offset tested it.
    Condition(usize),
    /// Where the negation at this offset read it.
    Negation(usize),
}

/// A type annotation whose builtin names may be respelled.
#[derive(Clone, Copy, Debug)]
pub enum Annotation<'a, 'n> {
    /// A parameter of `def`.
    Parameter(&'a Def),
    /// The declared result of `def`.
    Result(&'a Def, &'a TypeExpr),
    /// The property named by the token, in the class of that dotted name.
    Property(&'n str, Tok, &'a TypeExpr),
    /// A block parameter.
    BlockParameter,
}

/// What an implementor of [`Walk`](super::Walk) supplies: where findings go,
/// what it knows about the program's types, and rules of its own that run
/// as the walk reaches each construct.
///
/// Every method but [`Self::report`] has a default that knows nothing
/// beyond the syntax and adds no rules, which is what the compiler uses
/// where the static checker has no answer.
pub trait Hooks<'a>: DerefMut<Target = Surface<'a>> {
    /// Takes a finding the rules could not turn into a rewrite.
    fn report(&mut self, finding: Finding);

    /// Whether rewrites may use syntax only the ADR-007 compiler accepts.
    fn new_syntax(&self) -> bool {
        true
    }

    /// The kinds of value a member call's receiver has, as the rename table
    /// names them (`string`, `array`, `hash`, `nil`, `class Name`, `enum`,
    /// `any` and so on), when they are known.
    fn receiver_kinds(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let _ = (expr, call);
        None
    }

    /// Whether the receiver is known to be a value whose member of the
    /// call's name is called the same with or without parentheses: not a
    /// hash, which may hold a function under the name, and not `nil`.
    fn receiver_plain(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let _ = (expr, call);
        false
    }

    /// Whether the receiver is known to be a host value, such as a
    /// capability, whose own methods may be named like removed ones: some
    /// when the receiver's types are known, none when not.
    fn receiver_dynamic(&self, expr: &'a Expr, call: &'a Call) -> Option<bool> {
        let _ = (expr, call);
        None
    }

    /// Whether every hash the receiver held had data under the member's
    /// name: a dot read raises at a missing field and calls a function,
    /// where the index reads nil or the function.
    fn field_holds_data(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let _ = (expr, call);
        true
    }

    /// Whether the receiver is a hash with a field of the member's name,
    /// which answers the call instead.
    fn receiver_field(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let _ = (expr, call);
        false
    }

    /// The kinds the receiver's values had, among scalars and arrays, for
    /// a call written without parentheses on today's runtime.
    fn bare_call_kinds(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let _ = (expr, call);
        None
    }

    /// Whether every recorded call at this site returned. A rewrite of a
    /// call that raised could change the error it reports.
    fn call_returned(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let _ = (expr, call);
        true
    }

    /// Whether the value indexed at the `[` at `offset` is known to be a
    /// hash, which a symbol key should become a string key of: some when
    /// its types are known.
    fn index_is_hash(&self, offset: usize) -> Option<bool> {
        let _ = offset;
        None
    }

    /// Whether `Hash.new` at this site read a value the script stored on
    /// the namespace instead of making a hash.
    fn namespace_field(&self, expr: &'a Expr) -> bool {
        let _ = expr;
        false
    }

    /// Whether the target accepts a rename's replacement here.
    fn accepts_rewrite(&self, pattern: &Pattern, call: Option<&'a Call>) -> bool {
        let _ = (pattern, call);
        true
    }

    /// Whether an annotation's checks never failed, so respelling its
    /// builtin type names cannot change an error message.
    fn annotation_passed(&self, annotation: Annotation<'a, '_>) -> bool {
        let _ = annotation;
        true
    }

    /// The type to declare a keyword parameter written without one, such
    /// as `retries` in `retries: 3`, as it moves after a bare `*`: by
    /// default the type of its literal default, or none to leave it
    /// undeclared.
    fn keyword_type(&mut self, def: &'a Def, param: &'a Param) -> Option<String> {
        let _ = def;
        let default = param.default.as_ref()?;
        super::context::literal_type(self, default).map(str::to_owned)
    }

    /// Whether a condition's value is a `bool`.
    fn test(&self, expr: &'a Expr, probe: Probe) -> Test {
        let _ = (expr, probe);
        Test::Bool
    }

    /// Runs after an assignment's targets and values are walked.
    fn after_assign(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        let _ = (stmt, assign);
    }

    /// Runs after a binary operator's operands are walked.
    fn after_binary(
        &mut self,
        expr: &'a Expr,
        op: Tok,
        left: &'a Expr,
        right: &'a Expr,
        place: Place,
    ) {
        let _ = (expr, op, left, right, place);
    }

    /// Runs after a function's parameters are walked, before its body.
    fn before_body(&mut self, def: &'a Def, class: Option<&'a Class>) {
        let _ = (def, class);
    }

    /// Runs before a class's members are walked; `name` is its dotted name.
    fn before_class(&mut self, class: &'a Class, name: &str) {
        let _ = (class, name);
    }

    /// Runs after a `case` is walked.
    fn after_case(&mut self, node: &'a Case) {
        let _ = node;
    }
}
