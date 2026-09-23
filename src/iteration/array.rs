//! Call-shape checks for the block-driven array members. Each member checks
//! its positional arguments, keywords and block in the reference's order and
//! reports the reference's message, so the generic checks that follow in
//! `start` never reject an array call.

use super::{MethodKind, argument};
use crate::{Error, ErrorKind, Result, Value, value::Kind};

fn type_error(message: String) -> Error {
    Error::new(ErrorKind::Type, message)
}

/// Rejects a malformed call to the array member `name`. `block` reports
/// whether a block was attached.
pub(super) fn check(
    name: &str,
    method: MethodKind,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<()> {
    use MethodKind::*;
    // `collect_concat` is an alias the reference reports under `flat_map`.
    let label = if method == FlatMap { "flat_map" } else { name };
    let refuse =
        |problem: &str| -> Result<()> { Err(argument(&format!("array.{label} {problem}"))) };
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
        Each | Map | Select => needs_block(),
        EachIndex | MapIndex | FlatMap | FilterMap | SliceWhen | ChunkWhile | DeleteIf | KeepIf => {
            no_arguments()?;
            no_keywords()?;
            needs_block()
        }
        ReverseEach | Reject | TakeWhile | DropWhile | Partition | GroupBy | GroupStable => {
            no_arguments()?;
            needs_block()
        }
        EachSlice | EachCons => {
            let (expects, invalid) = if method == EachSlice {
                ("expects a slice size", "invalid slice size")
            } else {
                ("expects a window size", "invalid size")
            };
            let [size] = args else {
                return refuse(expects);
            };
            match size.0 {
                Kind::Int(n) if n > 0 => needs_block(),
                Kind::Int(_) => refuse(invalid),
                _ => Err(type_error(format!("array.{label} {invalid}"))),
            }
        }
        Cycle => {
            if args.len() > 1 {
                return refuse("accepts at most one count");
            }
            match args.first().map(|count| &count.0) {
                None | Some(Kind::Nil | Kind::Int(_)) => needs_block(),
                Some(Kind::Big(_)) => {
                    Err(type_error(format!("array.{label} count is out of range")))
                }
                Some(_) => Err(type_error(format!(
                    "array.{label} count must be an integer"
                ))),
            }
        }
        Find => {
            if args.len() > 1 || args.first().is_some_and(|v| !matches!(v.0, Kind::Nil)) {
                return refuse("takes no fallback; a miss returns nil");
            }
            no_keywords()?;
            needs_block()
        }
        Index | Rindex => {
            if block {
                if !args.is_empty() {
                    return refuse("takes a value or a block, not both");
                }
                return Ok(());
            }
            if args.is_empty() || args.len() > 2 {
                return refuse("expects a value (with optional offset) or a block");
            }
            match args.get(1).map(crate::sequence::integer) {
                Some(Ok(n)) if n < 0 => refuse("offset must be non-negative integer"),
                Some(Err(_)) => refuse("offset must be non-negative integer"),
                _ => Ok(()),
            }
        }
        Reduce => {
            no_keywords()?;
            if args.len() > 2 {
                return refuse("accepts at most an initial value and an operation");
            }
            let operation = match args {
                [_, operation] => Some(operation),
                [operation] if !block => Some(operation),
                [] if !block => return refuse("requires a block or an operation"),
                _ => None,
            };
            if operation.is_some_and(|operation| operation.as_bytes().is_none()) {
                return Err(type_error(format!(
                    "array.{label} operation must be a symbol or string"
                )));
            }
            Ok(())
        }
        Count => {
            if args.len() > 1 {
                return refuse("accepts at most one value argument");
            }
            Ok(())
        }
        Any | All | NoneMatch => {
            no_keywords()?;
            if args.len() > 1 {
                return refuse("accepts at most one value argument");
            }
            Ok(())
        }
        One | Tally => no_arguments(),
        ToHash | Uniq => {
            no_arguments()?;
            no_keywords()
        }
        Fetch => {
            if args.is_empty() || args.len() > 2 {
                return refuse("expects index and optional default");
            }
            let index = &args[0];
            if crate::sequence::integer(index).is_err() {
                return Err(type_error(format!("array.{label} index must be integer")));
            }
            if matches!(index.0, Kind::Float(f) if f.trunc() != f) {
                return refuse("index must be integer");
            }
            Ok(())
        }
        Sum => {
            no_keywords()?;
            if args.len() > 1 {
                return refuse("accepts at most an initial value");
            }
            Ok(())
        }
        Grep | GrepV => {
            if args.len() != 1 {
                return refuse("expects exactly one pattern argument");
            }
            Ok(())
        }
        Fill => {
            no_keywords()?;
            if !block && args.is_empty() {
                return refuse("requires a value or a block");
            }
            if args.len() - usize::from(!block) > 2 {
                return refuse("accepts at most a start and length");
            }
            Ok(())
        }
        Delete => {
            no_keywords()?;
            if args.len() != 1 {
                return refuse("expects exactly one value");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
