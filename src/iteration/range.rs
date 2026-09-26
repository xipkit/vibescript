//! Call-shape checks for the block-driven range members other than `step`,
//! `sum`, `min` and `max`, which `bounds` checks. Each member checks its
//! positional arguments, keywords and block in the reference's order and
//! reports the reference's message, so the generic checks that follow in
//! `start` never reject a range call.

use super::{MethodKind, argument};
use crate::{Result, Value, value::Kind};

/// Rejects a malformed call to the range member `name`. `block` reports
/// whether a block was attached.
pub(super) fn check(
    name: &str,
    method: MethodKind,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<()> {
    use MethodKind::*;
    let refuse =
        |problem: &str| -> Result<()> { Err(argument(&format!("range.{name} {problem}"))) };
    let no_keywords = || {
        if keywords {
            refuse("does not take keyword arguments")
        } else {
            Ok(())
        }
    };
    let needs_block = || {
        if block {
            Ok(())
        } else {
            refuse("requires a block")
        }
    };
    match method {
        Each | Map | Select | Reject | Count => {
            if !args.is_empty() {
                return refuse("does not take arguments");
            }
            no_keywords()?;
            needs_block()
        }
        Find => {
            const FALLBACK: &str = "takes no fallback; a miss returns nil";
            if args.len() > 1 || args.first().is_some_and(|v| !matches!(v.0, Kind::Nil)) {
                return refuse(FALLBACK);
            }
            no_keywords()?;
            needs_block()?;
            // The port also refuses the explicit nil fallback the reference
            // accepts.
            if !args.is_empty() {
                return refuse(FALLBACK);
            }
            Ok(())
        }
        Reduce => {
            if args.len() > 1 {
                return refuse("expects at most one argument");
            }
            no_keywords()?;
            needs_block()
        }
        _ => Ok(()),
    }
}
