//! Consumes only aliases whose loop results cannot be observed.

use super::{Function, Op};

pub(super) fn discarded(functions: &mut [Function]) {
    for function in functions {
        for ip in 0..function.code.len() {
            let Op::LoopStart {
                end, expression, ..
            } = function.code[ip]
            else {
                continue;
            };
            let end = end as usize;
            let following = function.code[end + 1..]
                .iter()
                .find(|op| !matches!(op, Op::Declare(_)));
            let unused = expression
                || matches!(following, Some(Op::Pop))
                || (function.returns_nil && matches!(following, Some(Op::Finish)));
            if unused && let Op::Shovel(site, _) = function.code[end - 2] {
                function.code[end - 2] = Op::Shovel(site, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Engine, Value};

    #[test]
    fn unused_array_loop_tails_consume_only_the_loop_alias() {
        let script = Engine::new().compile("def run -> array<int>\nout: array<int> = []\nfor i in 0...16\nnext if i%3==0\nout << i\nend\nout\nend").unwrap();
        let code = &script.inner.code.program.functions[1].code;
        assert!(
            code.iter().any(|op| matches!(op, Op::Shovel(_, true))),
            "{code:?}"
        );
        let value = script.call("run", &[], CallOptions::default()).unwrap();
        let expected: Vec<_> = (0..16).filter(|i| i % 3 != 0).map(Value::int).collect();
        assert_eq!(
            format!("{:?}", value.value),
            format!("{:?}", Value::array(expected))
        );
        // Ten retained elements and the existing array header, without a
        // power-of-two tail introduced by consuming the alias.
        assert_eq!(
            value.stats.retained_memory_bytes,
            10 * size_of::<Value>() + crate::value::Heap::<Value>::header_bytes()
        );
    }
}
