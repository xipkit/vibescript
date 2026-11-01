//! A reusable budget for compilation and the REPL's supporting passes.

use crate::{
    CallContext, CallOptions, Engine, Result, Script, Stats,
    compilation::{Meter, Work},
};
use std::cell::RefCell;

/// A cumulative work budget for parsing, compilation and type checking.
///
/// Every pass, including a failed one, spends the same step quota. Memory is
/// the compiler's tracked allocation account, not process RSS; returned scripts
/// and tooling results do not retain this budget. Exhaustion stays latched.
/// Create a fresh budget to start a new allowance.
pub struct CompileBudget {
    context: CallContext,
}

impl CompileBudget {
    /// Creates a budget from the limits, cancellation and deadline in `options`.
    /// Globals and capabilities are ignored.
    pub fn new(options: &CallOptions) -> Self {
        Self {
            context: CallContext::new(CallOptions {
                globals: Default::default(),
                capabilities: Vec::new(),
                limits: options.limits.clone(),
                cancellation: options.cancellation.clone(),
                deadline: options.deadline,
                allow_require: options.allow_require,
            }),
        }
    }

    /// Returns the work spent and the peak tracked compiler memory so far.
    pub fn stats(&self) -> Stats {
        self.context.stats()
    }

    /// Compiles a script, spending this budget even when compilation fails.
    pub fn compile(&mut self, engine: &Engine, source: &str) -> Result<Script> {
        let code = crate::code::Code::compile_metered(
            source,
            &engine.hosts,
            &engine.declared,
            &Meter(RefCell::new(&mut self.context)),
            &engine.loader,
        )?;
        self.context.checkpoint()?;
        Ok(engine.script(code))
    }

    /// Declares a retained script class, module or enum on `engine`, charging
    /// the parse and dependency search of its original namespace source.
    /// Other kinds of value are rejected. Returned engine metadata is not kept
    /// in the compiler memory account after this method returns.
    pub fn retain_namespace(
        &mut self,
        engine: &mut Engine,
        name: &str,
        value: &crate::Value,
    ) -> Result<()> {
        let declaration = crate::declared::Declaration::retained_value(
            name,
            value,
            &Meter(RefCell::new(&mut self.context)),
        )?;
        self.context.checkpoint()?;
        std::sync::Arc::make_mut(&mut engine.declared).insert(name.to_owned(), declaration);
        engine.loader = std::sync::Arc::new(engine.loader.fresh());
        Ok(())
    }

    /// Parses the names of top-level functions, aliases, classes, modules and
    /// enums without checking their types. Names are grouped by declaration kind.
    pub fn declaration_names(&mut self, source: &str) -> Result<Vec<String>> {
        let work = Meter(RefCell::new(&mut self.context));
        work.checkpoint()?;
        let parsed = crate::syntax::parse(source, &work).map_err(|error| {
            crate::source::parse_error(
                source,
                None,
                crate::syntax::host_syntax(source, &work, error),
                &work,
            )
        })?;
        let mut names = Vec::new();
        for name in parsed
            .functions
            .iter()
            .skip(1)
            .map(|f| &f.name)
            .chain(parsed.modules.iter().map(|m| &m.name))
            .chain(parsed.enums.iter().map(|(name, _)| name))
        {
            work.bytes(name.len().saturating_add(size_of::<String>()))?;
            names.push(name.to_string());
        }
        work.checkpoint()?;
        Ok(names)
    }

    /// Type checks source and returns its diagnostics and session annotations.
    /// A quota failure or interruption returns an error, never partial findings.
    pub fn type_check(&mut self, engine: &Engine, source: &str) -> Result<crate::typing::Checked> {
        let work = Meter(RefCell::new(&mut self.context));
        work.checkpoint()?;
        for name in engine.hosts.keys() {
            crate::syntax::host_function_name(&work, crate::syntax::HostName::FUNCTION, name)?;
        }
        let (parsed, tokens, _held) =
            crate::syntax::parse_with_tokens(source, &work).map_err(|error| {
                crate::source::parse_error(
                    source,
                    None,
                    crate::syntax::canonical_syntax(source, &work, error),
                    &work,
                )
            })?;
        let mut hosts = crate::compilation::Buffer::with_capacity(&work, engine.hosts.len())?;
        for host in &engine.hosts {
            hosts.push(&work, host)?;
        }
        let resolve = |path: &str, origin: Option<&crate::loading::Origin>, ctx: &mut _| {
            engine.loader.source(path, origin, ctx)
        };
        let checked = crate::typing::check(&crate::typing::Input {
            source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: &hosts,
            declared: &engine.declared,
            file: false,
            origin: None,
            modules: Some(&resolve),
            budget: work.budget(),
            observe: None,
            annotate: true,
        });
        work.charge(usize::try_from(checked.steps).unwrap_or(usize::MAX))?;
        work.checkpoint()?;
        if checked.stopped {
            return Err(work.allocation_error("memory quota exceeded while checking types"));
        }
        drop(work.reserve(checked.peak())?);
        drop(work.reserve(checked.bytes())?);
        Ok(checked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, ErrorKind, Limits};

    fn unlimited() -> CallOptions {
        CallOptions {
            limits: Limits {
                steps: None,
                memory_bytes: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        }
    }

    #[test]
    fn passes_share_one_step_allowance_and_latch_exhaustion() {
        let engine = Engine::new();
        let mut baseline = CompileBudget::new(&unlimited());
        baseline.compile(&engine, "1").unwrap();
        let steps = baseline.stats().steps;
        let mut budget = CompileBudget::new(&CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        budget.compile(&engine, "1").unwrap();
        assert_eq!(budget.stats().steps, steps);
        let error = budget.declaration_names("def f; 1; end").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(budget.type_check(&engine, "1").unwrap_err(), error);
        assert_eq!(budget.compile(&engine, "1").err().unwrap(), error);
    }

    #[test]
    fn metered_session_annotations_match_unmetered_checker() {
        let engine = Engine::new();
        let source = "def double(n: int) -> int; n * 2; end; x = double(2); x";
        let expected = engine.type_check(source).unwrap();
        let mut budget = CompileBudget::new(&unlimited());
        assert_eq!(budget.declaration_names(source).unwrap(), ["double"]);
        let parsed = budget.stats().steps;
        let checked = budget.type_check(&engine, source).unwrap();
        assert_eq!(checked.locals, expected.locals);
        assert_eq!(checked.result, expected.result);
        assert_eq!(checked.diagnostics, expected.diagnostics);
        assert!(budget.stats().steps > parsed);
        assert!(budget.stats().peak_memory_bytes > 0);
        // Failed parses also spend this same allowance.
        let checked_steps = budget.stats().steps;
        assert!(budget.declaration_names("def unfinished(").is_err());
        assert!(budget.stats().steps > checked_steps);
    }

    #[test]
    fn every_pass_observes_memory_deadline_and_cancellation() {
        let engine = Engine::new();
        let token = CancellationToken::new();
        token.cancel();
        for (options, kind) in [
            (
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(0),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
                ErrorKind::Memory,
            ),
            (
                CallOptions {
                    deadline: Some(std::time::Instant::now()),
                    ..CallOptions::default()
                },
                ErrorKind::Deadline,
            ),
            (
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                },
                ErrorKind::Cancelled,
            ),
        ] {
            assert_eq!(
                CompileBudget::new(&options)
                    .compile(&engine, "1")
                    .err()
                    .unwrap()
                    .kind,
                kind
            );
            assert_eq!(
                CompileBudget::new(&options)
                    .type_check(&engine, "1")
                    .unwrap_err()
                    .kind,
                kind
            );
            assert_eq!(
                CompileBudget::new(&options)
                    .declaration_names("def f; 1; end")
                    .unwrap_err()
                    .kind,
                kind
            );
        }
    }
}

#[cfg(test)]
mod retained_tests {
    use super::*;
    use crate::{CancellationToken, Error, ErrorKind, budget::Charge};
    use std::cell::Cell;

    struct InterruptDuringParse<'a> {
        work: Meter<'a>,
        remaining: Cell<usize>,
        token: CancellationToken,
    }

    impl Work for InterruptDuringParse<'_> {
        fn budget(&self) -> crate::compilation::Budget {
            self.work.budget()
        }
        fn charge(&self, steps: usize) -> Result<()> {
            self.work.charge(steps)
        }
        fn bytes(&self, bytes: usize) -> Result<()> {
            let left = self.remaining.get().saturating_sub(bytes);
            self.remaining.set(left);
            if left == 0 {
                self.token.cancel();
            }
            self.work.bytes(bytes)
        }
        fn checkpoint(&self) -> Result<()> {
            self.work.checkpoint()
        }
        fn reserve(&self, bytes: usize) -> Result<Option<Charge>> {
            self.work.reserve(bytes)
        }
        fn allocation_error(&self, message: &str) -> Error {
            self.work.allocation_error(message)
        }
    }

    #[test]
    fn retained_namespace_preparation_is_metered_and_interruptible() {
        let source = format!(
            "# {}\nclass A; end\nclass B; end",
            "padding ".repeat(20_000)
        );
        let engine = Engine::new();
        let (_, bindings) = engine
            .compile(&source)
            .unwrap()
            .run_bindings(CallOptions::default())
            .unwrap();
        let value = &bindings["A"];
        for (options, kind) in [
            (
                CallOptions {
                    limits: crate::Limits {
                        steps: Some(1),
                        ..crate::Limits::default()
                    },
                    ..CallOptions::default()
                },
                ErrorKind::Steps,
            ),
            (
                CallOptions {
                    limits: crate::Limits {
                        memory_bytes: Some(0),
                        ..crate::Limits::default()
                    },
                    ..CallOptions::default()
                },
                ErrorKind::Memory,
            ),
            (
                CallOptions {
                    deadline: Some(std::time::Instant::now()),
                    ..CallOptions::default()
                },
                ErrorKind::Deadline,
            ),
        ] {
            let mut receiving = Engine::new();
            let error = CompileBudget::new(&options)
                .retain_namespace(&mut receiving, "A", value)
                .unwrap_err();
            assert_eq!(error.kind, kind);
            assert!(receiving.compile("A.new").is_err());
        }
        let token = CancellationToken::new();
        let mut context = CallContext::new(CallOptions {
            cancellation: token.clone(),
            ..CallOptions::default()
        });
        let work = InterruptDuringParse {
            work: Meter(RefCell::new(&mut context)),
            remaining: Cell::new(1024),
            token,
        };
        let error = crate::declared::Declaration::retained_value("A", value, &work).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(work.remaining.get(), 0);
        assert!(context.stats().steps > 0);
        // This API accepts only the namespace/enum preparation needed by sessions.
        assert_eq!(
            CompileBudget::new(&CallOptions::default())
                .retain_namespace(&mut Engine::new(), "x", &crate::Value::int(1))
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
    }
}
