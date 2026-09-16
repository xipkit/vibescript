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
        for at in [1, baseline.visits.get() / 2, baseline.visits.get() - 1] {
            let interrupted = Interrupt::new(at, false);
            assert_eq!(run(&interrupted).unwrap_err().kind, ErrorKind::Cancelled);
            assert_eq!(interrupted.visits.get(), at + 1);
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
    assert!(steps(&aliased) >= base + declarations + 2 * 32 * 512);
    let mut options = CallOptions::default();
    options.limits.steps = Some(base + declarations + 32 * 512);
    let mut context = CallContext::new(options);
    let result = crate::syntax::parse(&aliased, &Meter(RefCell::new(&mut context)));
    assert_eq!(result.err().unwrap().kind, ErrorKind::Steps);
}
