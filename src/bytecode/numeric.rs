use super::*;

impl Compiler<'_> {
    fn number_pair(&self, a: &Expr, b: &Expr) -> bool {
        self.facts
            .number(a)
            .is_some_and(|number| self.facts.number(b) == Some(number))
    }

    /// Emits numeric arithmetic without a dynamic operator-overload probe.
    pub(super) fn number_binary(&mut self, op: &str, a: &Expr, b: &Expr) -> Result<usize> {
        let number = self.number_pair(a, b);
        let instruction = self.binary(op)?;
        let Op::Binary(operator, _) = self.code[instruction] else {
            unreachable!()
        };
        self.code[instruction] = Op::Binary(operator, number);
        Ok(instruction)
    }

    /// Emits a fused addition with the same proof about its operands.
    pub(super) fn number_store(&mut self, slot: usize, a: &Expr, b: &Expr) -> usize {
        self.emit(Op::AddStore(narrow(slot), self.number_pair(a, b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Engine, Limits, Script};
    use std::sync::Arc;

    fn general(script: Script) -> (Script, usize) {
        let inner = Arc::try_unwrap(script.inner).ok().unwrap();
        let mut code = Arc::try_unwrap(inner.code).ok().unwrap();
        let program = &mut code.program;
        let mut count = 0;
        for function in &mut program.functions {
            for op in &mut function.code {
                *op = match *op {
                    Op::Binary(operator, true) => {
                        count += 1;
                        Op::Binary(operator, false)
                    }
                    Op::AddStore(slot, true) => {
                        count += 1;
                        Op::AddStore(slot, false)
                    }
                    op => op,
                };
            }
        }
        let code = Arc::new_cyclic(|owner| {
            code.program.owner = owner.clone();
            code
        });
        (
            Script {
                inner: Arc::new(crate::ScriptInner { code, ..inner }),
            },
            count,
        )
    }

    fn compare(source: &str) {
        let specialized = Engine::new().compile(source).unwrap();
        let (reference, count) = general(Engine::new().compile(source).unwrap());
        assert!(count > 0, "no specialization: {source}");
        let actual = specialized.run(CallOptions::default());
        let expected = reference.run(CallOptions::default());
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"), "{source}");
        let (steps, peak) = expected.map_or((80, 1024), |out| {
            (out.stats.steps, out.stats.peak_memory_bytes)
        });
        for limit in 0..=steps {
            let options = || CallOptions {
                limits: Limits {
                    steps: Some(limit),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            let actual = specialized.run(options());
            let expected = reference.run(options());
            assert_eq!(
                format!("{actual:?}"),
                format!("{expected:?}"),
                "{source}; steps={limit}"
            );
        }
        for limit in (0..=peak)
            .step_by(8)
            .chain([peak.saturating_sub(1), peak, peak + 1])
        {
            let options = || CallOptions {
                limits: Limits {
                    memory_bytes: Some(limit),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            let actual = specialized.run(options());
            let expected = reference.run(options());
            assert_eq!(
                format!("{actual:?}"),
                format!("{expected:?}"),
                "{source}; bytes={limit}"
            );
        }
    }

    #[test]
    fn specialization_preserves_values_errors_and_every_step_boundary() {
        for (ty, a, b) in [
            ("int", "-7", "3"),
            ("int", "9223372036854775807", "1"),
            ("int", "-9223372036854775808", "-1"),
            ("int", "9007199254740993", "3"),
            ("int", "9223372036854775808", "0"),
            ("float", "-0.0", "0.0"),
            ("float", "0.0/0.0", "1.0/0.0"),
            ("float", "0.0/0.0", "0.0/0.0"),
            ("float", "-3.5", "2.0"),
        ] {
            for op in [
                "+", "-", "*", "/", "//", "%", "==", "!=", "<", "<=", ">", ">=", "<=>",
            ] {
                compare(&format!("a: {ty} = {a}\nb: {ty} = {b}\na {op} b"));
            }
        }
        for source in [
            "x=9223372036854775807\nx+=1\nx-=1\nx",
            "x=0.25\nx+=0.5\nx+0.0",
            "n=9223372036854775807\n[1,2,3].each { |v| n+=v }\nn-0",
            "x=0\ni=0\nwhile i<8\nx+=1 if i%3==0 && i != 3\ni+=1\nend\nx",
            "def f(x: any) -> int\nif x.is_type?(:int)\nx+1\nelse\n0\nend\nend\nf(3)",
        ] {
            compare(source);
        }
    }

    #[test]
    fn numeric_unions_keep_general_operations() {
        let script = Engine::new()
            .compile("def add(x: number, y: number) -> number\nx+y\nend\n[add(1,2),add(0.25,0.5)]")
            .unwrap();
        assert_eq!(general(script).1, 0);
    }
}
