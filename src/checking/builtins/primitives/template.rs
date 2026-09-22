//! `String#template(context, strict: bool)` without rendering the template.
//!
//! The runtime scans `{{ key }}` placeholders, resolves each dotted key through
//! nested hashes and converts the selected value to text. Literal templates use
//! the runtime's own scanner, so analysis reads exactly the keys execution reads.

use super::{
    super::native::{keyword, name},
    *,
};

/// How the runtime's placeholder conversion treats one value alternative.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Accepted,
    Rejected,
    Gradual,
    Unsupported,
}

fn scalar(facts: &Facts, value: Fact) -> Scalar {
    match facts.node(value) {
        Node::Atom(
            Atom::Nil
            | Atom::Bool
            | Atom::Int
            | Atom::Float
            | Atom::String
            | Atom::Symbol
            | Atom::Duration
            | Atom::Time
            | Atom::Money,
        )
        | Node::Boolean(_)
        | Node::Integer(_)
        | Node::IntegerBounds(_)
        | Node::Float(_)
        | Node::String(_)
        | Node::Symbol(_)
        | Node::EnumMember { .. }
        | Node::Nominal {
            symbols: Some(_), ..
        } => Scalar::Accepted,
        Node::Atom(Atom::Unknown | Atom::Any) => Scalar::Gradual,
        Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) | Node::Union(_) => {
            Scalar::Unsupported
        }
        _ => Scalar::Rejected,
    }
}

/// The values one dotted placeholder key can select.
struct Selection {
    values: Fact,
    missing: bool,
    unsupported: bool,
}

/// Resolves `path` the way the runtime's template lookup does: every segment
/// reads a stored hash entry, and a non-hash value or an empty segment ends
/// the lookup as a missing placeholder.
fn select(
    ctx: &mut CallContext,
    facts: &mut Facts,
    context: Fact,
    path: &[u8],
) -> Result<Selection> {
    let mut selection = Selection {
        values: context,
        missing: false,
        unsupported: false,
    };
    for segment in path.split(|&byte| byte == b'.') {
        ctx.work_bytes(segment.len().saturating_add(1))?;
        let mut values = Buffer::empty();
        for i in 0..facts.arm_count(selection.values) {
            ctx.charge(1)?;
            let arm = facts.arm(selection.values, i);
            let view = match facts.node(arm) {
                Node::Protected(shape, ..) => *shape,
                _ => arm,
            };
            match facts.node(view) {
                Node::Atom(Atom::Never) => (),
                _ if segment.is_empty() => selection.missing = true,
                Node::Shape(_, open, ..) => {
                    let open = *open;
                    match facts.selected_field(ctx, view, segment)? {
                        Some((value, optional)) => {
                            values.push(ctx, value)?;
                            selection.missing |= optional;
                        }
                        None if open => {
                            values.push(ctx, Atom::Unknown.fact())?;
                            selection.missing = true;
                        }
                        None => selection.missing = true,
                    }
                }
                Node::Hash(_, value, _) => {
                    values.push(ctx, *value)?;
                    selection.missing = true;
                }
                Node::Atom(Atom::Unknown | Atom::Any) => {
                    values.push(ctx, Atom::Unknown.fact())?;
                    selection.missing = true;
                }
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                    selection.unsupported = true;
                }
                _ => selection.missing = true,
            }
        }
        selection.values = facts.union(ctx, &values.data)?;
    }
    Ok(selection)
}

/// Models `template` after the caller has checked its single positional
/// argument. The context must be a hash or object; `strict:` must be the only
/// keyword and must be boolean. A strict template fails when a key is missing,
/// and every selected value must be a scalar that the runtime converts to text.
pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    args: &Arguments,
) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    let context = args.positional.data[0];
    for i in 0..facts.arm_count(context) {
        ctx.charge(1)?;
        let arm = facts.arm(context, i);
        if matches!(
            facts.node(arm),
            Node::Named(_) | Node::Nominal { .. } | Node::Choice(_)
        ) {
            result.incomplete = true;
        }
    }
    let mut possible = domain(ctx, facts, &mut result, context, |_, facts, arm| {
        Ok(match facts.node(arm) {
            Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => Some(true),
            Node::Atom(Atom::Unknown | Atom::Any)
            | Node::Named(_)
            | Node::Nominal { .. }
            | Node::Choice(_) => None,
            _ => Some(false),
        })
    })?;
    // The runtime reads only a single `strict:` keyword; its value selects the mode.
    let (mut lenient, mut strict) = (true, false);
    match args.keywords.data.as_slice() {
        [] => (),
        [option] => match name(facts, option.name) {
            Some(b"strict") => {
                possible &= keyword(ctx, facts, &mut result, *option, Atom::Bool.fact())?;
                (lenient, strict) = (false, false);
                for i in 0..facts.arm_count(option.value) {
                    ctx.charge(1)?;
                    match facts.node(facts.arm(option.value, i)) {
                        Node::Boolean(value) => {
                            lenient |= !value;
                            strict |= value;
                        }
                        Node::Atom(Atom::Bool | Atom::Unknown | Atom::Any) => {
                            (lenient, strict) = (true, true);
                        }
                        _ => (),
                    }
                }
            }
            Some(_) => {
                result
                    .failures
                    .push(ctx, Failure::BuiltinKeyword(option.name))?;
                result.throws |= RUNTIME;
                possible = false;
            }
            None => {
                result.throws |= RUNTIME;
                strict = true;
            }
        },
        _ => {
            result.failures.push(ctx, Failure::BuiltinKeywords)?;
            result.throws |= RUNTIME;
            possible = false;
        }
    }
    let Node::String(text) = facts.node(receiver) else {
        // Unknown placeholders may select missing or non-scalar values.
        result.throws |= RUNTIME;
        if possible {
            result.value = Atom::String.fact();
        }
        return Ok(result);
    };
    let text = text.clone();
    let bytes = text.as_bytes().unwrap();
    let mut scan = 0;
    let mut found = false;
    while let Some(placeholder) = crate::text::template::next(ctx, bytes, &mut scan)? {
        found = true;
        let selection = select(ctx, facts, context, &bytes[placeholder.key])?;
        result.incomplete |= selection.unsupported;
        if selection.missing && strict {
            result.throws |= RUNTIME;
        }
        let mut converts = selection.missing && lenient;
        let mut rejected = false;
        for i in 0..facts.arm_count(selection.values) {
            ctx.charge(1)?;
            let arm = facts.arm(selection.values, i);
            if arm == Atom::Never.fact() {
                continue;
            }
            match scalar(facts, arm) {
                Scalar::Accepted => converts = true,
                Scalar::Gradual => {
                    converts = true;
                    result.throws |= RUNTIME;
                }
                Scalar::Rejected => rejected = true,
                Scalar::Unsupported => result.incomplete = true,
            }
        }
        if rejected {
            result
                .failures
                .push(ctx, Failure::BuiltinDomain(selection.values))?;
            result.throws |= RUNTIME;
        }
        if !converts {
            if selection.missing && !rejected {
                // Every selected path is missing and the call is certainly strict.
                result
                    .failures
                    .push(ctx, Failure::BuiltinDomain(receiver))?;
            }
            possible = false;
        }
    }
    if possible {
        // Without placeholders the runtime returns the receiver itself.
        result.value = if found { Atom::String.fact() } else { receiver };
    }
    Ok(result)
}
