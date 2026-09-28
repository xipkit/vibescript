//! Call-shape checks for the block-driven hash members. Each member checks
//! its positional arguments, keywords and block in the reference's order and
//! reports the reference's message, so the generic checks that follow in
//! `start` never reject a hash call.

use super::{MethodKind, argument};
use crate::{Result, Value};

/// Rejects a malformed call to the hash member `name`. `block` reports
/// whether a block was attached.
pub(super) fn check(
    name: &str,
    method: MethodKind,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<()> {
    use MethodKind::*;
    let refuse = |problem: &str| -> Result<()> { Err(argument(&format!("hash.{name} {problem}"))) };
    let no_arguments = || {
        if args.is_empty() {
            Ok(())
        } else {
            refuse("does not take arguments")
        }
    };
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
        Each | EachKey | EachValue | Select | Reject | TransformKeys | TransformValues => {
            no_arguments()?;
            needs_block()
        }
        EachIndex | Map | MapIndex | DeleteIf | KeepIf => {
            no_arguments()?;
            no_keywords()?;
            needs_block()
        }
        Fetch if args.is_empty() || args.len() > 2 => refuse("expects key and optional default"),
        // Without a block, delete reaches the mutator, which also names a
        // bare read of the member.
        Delete if block => {
            if keywords {
                return refuse("does not accept keyword arguments");
            }
            if args.len() != 1 {
                return refuse("expects a key");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
