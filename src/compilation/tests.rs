use super::*;
use crate::{CallOptions, ErrorKind};
use std::{cell::Cell, time::Instant};

struct Interrupt {
    context: RefCell<CallContext>,
    visits: Cell<usize>,
    at: usize,
    deadline: bool,
}

impl Interrupt {
    fn new(at: usize, deadline: bool) -> Self {
        let mut options = CallOptions::default();
        options.limits.steps = None;
        Self {
            context: RefCell::new(CallContext::new(options)),
            visits: Cell::new(0),
            at,
            deadline,
        }
    }

    fn visit(&self) -> Result<()> {
        let mut context = self.context.borrow_mut();
        let visits = self.visits.get();
        self.visits.set(visits + 1);
        if visits >= self.at {
            if self.deadline {
                context.options.deadline = Some(Instant::now());
            } else {
                context.cancellation().cancel();
            }
        }
        context.checkpoint()
    }
}

impl Work for Interrupt {
    fn charge(&self, steps: usize) -> Result<()> {
        self.visit()?;
        self.context.borrow_mut().charge(steps as u64)
    }

    fn bytes(&self, bytes: usize) -> Result<()> {
        self.visit()?;
        self.context.borrow_mut().work_bytes(bytes)
    }

    fn checkpoint(&self) -> Result<()> {
        self.context.borrow_mut().checkpoint()
    }

    fn reserve(&self, bytes: usize) -> Result<Option<crate::budget::Charge>> {
        self.visit()?;
        self.context.borrow_mut().reserve(bytes)
    }

    fn allocation_error(&self, message: &str) -> crate::Error {
        let mut context = self.context.borrow_mut();
        context
            .checkpoint()
            .err()
            .unwrap_or_else(|| context.fail::<()>(ErrorKind::Memory, message).unwrap_err())
    }
}

fn sources() -> Vec<String> {
    vec![
        format!("def unused;{}42;end", "[1,2,3].map{|n|n+1};".repeat(64)),
        format!("#{}\n42", "comment ".repeat(1024)),
        format!("\"{}\"", "日本語\\n".repeat(1024)),
        format!("%W[{}]", "x#{1+2} ".repeat(128)),
        format!("/[a-z]{}+/im", "abc".repeat(512)),
        format!("0x{}", "abc123".repeat(512)),
        format!("0.{}", "123456".repeat(512)),
        format!("w=[3];total=10;n=0;{}n", "n+=total %w[0];".repeat(128)),
        format!(
            "enum Status;{};end",
            (0..128)
                .map(|i| format!("Member{i}"))
                .collect::<Vec<_>>()
                .join(";")
        ),
        format!(
            "module Outer;def self.value;1;end;end;class Box;property value;{}end",
            (0..64)
                .map(|i| format!("def m{i}(n:int=1)->int;n;end;"))
                .collect::<String>()
        ),
        format!(
            "schema={{{}}};schema",
            (0..128)
                .map(|i| format!("f{i}:array<int>"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!("schema={}int{};schema", "{x:".repeat(63), "}".repeat(63)),
        "def take(a:int|nil=nil,b:{id:int}={id:1});[a,b];end;take(b:{id:2})".into(),
        "module M;Hash[:n]=1;Regexp[:n]=2;Regex[:n]=3;Time[:n]=4;Duration[:n]=5;JSON[:n]=6;Math[:n]=7;end;0".into(),
        "w={\"]\":3};n=10;n %w[\"]\"];17".into(),
        "def f(x);begin;x;rescue ArgumentError|TypeError=>e;raise e;ensure;nil;end;end".into(),
        format!("x=1\n{}+ 2", "\n".repeat(1024)),
        format!(
            "class Box;def original(n:int=1);{}end;alias copied original;end",
            "begin;x=[1,2,3].map{|a|a+n};rescue=>e;raise e;ensure;nil;end;".repeat(32)
        ),
    ]
}

#[test]
fn compilation_limits_cover_parsing_and_code_generation() {
    for source in sources() {
        for bytecode in [false, true] {
            let run = |work: &dyn Work| {
                if bytecode {
                    crate::bytecode::compile_file(&source, Vec::new(), work).map(|_| ())
                } else {
                    crate::syntax::parse(&source, work).map(|_| ())
                }
            };
            let baseline = Interrupt::new(usize::MAX, false);
            run(&baseline).unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
            let steps = baseline.context.borrow().stats().steps;
            let mut options = CallOptions::default();
            options.limits.steps = Some(steps);
            let mut exact = CallContext::new(options.clone());
            run(&Meter(RefCell::new(&mut exact))).unwrap();
            for limit in [0, 1, steps / 4, steps / 2, steps - 1] {
                options.limits.steps = Some(limit);
                let mut context = CallContext::new(options.clone());
                let error = run(&Meter(RefCell::new(&mut context))).unwrap_err();
                assert_eq!(
                    error.kind,
                    ErrorKind::Steps,
                    "bytecode={bytecode}, limit={limit}: {source}"
                );
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Steps);
                assert_eq!(context.stats().retained_memory_bytes, 0);
            }
            let visits = baseline.visits.get();
            for at in [0, 1, visits / 4, visits / 2, visits - 1] {
                for deadline in [false, true] {
                    let interrupted = Interrupt::new(at, deadline);
                    let error = run(&interrupted).unwrap_err();
                    let kind = if deadline {
                        ErrorKind::Deadline
                    } else {
                        ErrorKind::Cancelled
                    };
                    assert_eq!(error.kind, kind, "bytecode={bytecode}, at={at}: {source}");
                    assert_eq!(interrupted.checkpoint().unwrap_err().kind, kind);
                    assert_eq!(interrupted.visits.get(), at + 1);
                    assert_eq!(
                        interrupted.context.borrow().stats().retained_memory_bytes,
                        0
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_speculation_preserves_termination() {
    for source in [
        format!("x=1;x %w[{}", "a ".repeat(512)),
        format!("x=\"{}", "\\n".repeat(512)),
        format!("schema={{ {}", "field:array<int>,".repeat(128)),
        format!("def f(n:int{}", "|string".repeat(128)),
    ] {
        let run = |work: &dyn Work| crate::bytecode::compile_file(&source, Vec::new(), work);
        let baseline = Interrupt::new(usize::MAX, false);
        assert_eq!(run(&baseline).unwrap_err().kind, ErrorKind::Syntax);
        assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
        for at in [1, baseline.visits.get() / 2, baseline.visits.get() - 1] {
            let interrupted = Interrupt::new(at, false);
            assert_eq!(run(&interrupted).unwrap_err().kind, ErrorKind::Cancelled);
            assert_eq!(interrupted.visits.get(), at + 1);
            assert_eq!(
                interrupted.context.borrow().stats().retained_memory_bytes,
                0
            );
        }
    }
}

#[test]
fn alias_work_includes_copied_method_bodies_before_code_generation() {
    let source = |statements: usize, aliases: usize| {
        format!(
            "class Box;def original;{}end;{}end",
            "1;".repeat(statements),
            (0..aliases)
                .map(|i| format!("alias copied{i} original;"))
                .collect::<String>(),
        )
    };
    let steps = |source: &str| {
        let work = Interrupt::new(usize::MAX, false);
        crate::syntax::parse(source, &work).unwrap();
        work.context.borrow().stats().steps
    };
    let base = steps(&source(512, 0));
    let declarations = steps(&source(0, 32)) - steps(&source(0, 0));
    let aliased = source(512, 32);
    assert!(steps(&aliased) >= base + 2 * 32 * 512);
    let mut options = CallOptions::default();
    options.limits.steps = Some(base + declarations + 32 * 512);
    let mut context = CallContext::new(options);
    let result = crate::syntax::parse(&aliased, &Meter(RefCell::new(&mut context)));
    assert_eq!(result.err().unwrap().kind, ErrorKind::Steps);
}

#[test]
fn parser_storage_is_bounded_and_released_with_its_output() {
    for source in sources() {
        let mut context = CallContext::new(CallOptions::default());
        let parsed = crate::syntax::parse(&source, &Meter(RefCell::new(&mut context)))
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        let peak = context.stats().peak_memory_bytes;
        assert!(peak > 0, "{source}");
        drop(parsed);
        assert_eq!(context.stats().retained_memory_bytes, 0, "{source}");
        for limit in [0, 1, peak / 2, peak - 1, peak] {
            let mut options = CallOptions::default();
            options.limits.memory_bytes = Some(limit);
            let mut context = CallContext::new(options);
            let result = crate::syntax::parse(&source, &Meter(RefCell::new(&mut context)));
            if limit == peak {
                assert!(result.is_ok(), "limit={limit}: {source}");
                drop(result);
            } else {
                assert_eq!(
                    result.err().unwrap().kind,
                    ErrorKind::Memory,
                    "limit={limit}: {source}"
                );
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            }
            assert_eq!(
                context.stats().retained_memory_bytes,
                0,
                "limit={limit}: {source}"
            );
        }
    }
}

#[test]
fn owned_literal_payloads_obey_limits_and_outlive_the_token_stream() {
    let length = 4096;
    for source in [
        format!("\"{}\"", "x".repeat(length)),
        format!("\"{}\"", "\\xff".repeat(length)),
        format!(":\"{}\"", "x".repeat(length)),
        format!("/{}/im", "x".repeat(length)),
        "9".repeat(length),
        format!("%w[{}]", "x".repeat(length)),
        format!("%W[{}#{{1}}]", "x".repeat(length)),
    ] {
        let mut context = CallContext::new(CallOptions::default());
        let memory = std::sync::Arc::downgrade(&context.identity());
        let parsed = crate::syntax::parse(&source, &Meter(RefCell::new(&mut context))).unwrap();
        let peak = context.stats().peak_memory_bytes;
        assert!(context.stats().retained_memory_bytes >= length, "{source}");
        drop(context);
        assert!(memory.upgrade().is_some());
        drop(parsed);
        assert!(memory.upgrade().is_none());
        for limit in [length / 2, peak - 1, peak] {
            let mut options = CallOptions::default();
            options.limits.memory_bytes = Some(limit);
            let mut context = CallContext::new(options);
            let result = crate::syntax::parse(&source, &Meter(RefCell::new(&mut context)));
            if limit == peak {
                assert!(result.is_ok(), "limit={limit}: {source}");
                drop(result);
            } else {
                assert_eq!(result.err().unwrap().kind, ErrorKind::Memory);
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            }
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn owned_syntax_containers_outlive_the_parser() {
    let mut context = CallContext::new(CallOptions::default());
    let memory = std::sync::Arc::downgrade(&context.identity());
    let mut parsed = crate::syntax::parse(
        "def first(n=1);[n+2,n*3][0];end;def second;[4,5];end",
        &Meter(RefCell::new(&mut context)),
    )
    .unwrap();
    let before = context.stats().retained_memory_bytes;
    let first = parsed.functions.remove(1);
    drop(parsed);
    let retained = context.stats().retained_memory_bytes;
    assert!(retained > 0 && retained < before);
    drop(context);
    assert!(memory.upgrade().is_some());
    drop(first);
    assert!(memory.upgrade().is_none());
}

#[test]
fn alias_compilation_preserves_supported_tree_depth() {
    for depth in [1, 32, 63] {
        let source = format!(
            "class Box;def original;{}1{};end;alias copied original;end;Box.new.copied",
            "[".repeat(depth),
            "]".repeat(depth),
        );
        let mut context = CallContext::new(CallOptions::default());
        let code =
            crate::bytecode::compile(&source, Vec::new(), &Meter(RefCell::new(&mut context)))
                .unwrap();
        assert_eq!(context.stats().retained_memory_bytes, 0);
        drop(code);
    }
}
