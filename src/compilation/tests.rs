use super::*;
use crate::{CallOptions, ErrorKind};
use std::{cell::Cell, time::Instant};

struct Interrupt {
    context: RefCell<CallContext>,
    visits: Cell<usize>,
    largest_bytes: Cell<usize>,
    byte_visits: Cell<usize>,
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
            largest_bytes: Cell::new(0),
            byte_visits: Cell::new(0),
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
        self.largest_bytes.set(self.largest_bytes.get().max(bytes));
        self.byte_visits.set(self.byte_visits.get() + 1);
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
        "class Box;property records:array<{name:string,value?:object<symbol,int|string>,...}>;alias copied records;end".into(),
        format!("def {}({}:int)->int;{};end", "name".repeat(1024), "value".repeat(512), "value".repeat(512)),
        format!("def take(value:{{{}:array<int>}});value;end", "field".repeat(1024)),
        "module Outer;module Inner;def self.value=(n:int);n;end;end;end;class Box;property value:int;alias_method(:\"日本語\", :value);end".into(),
        format!(
            "def f;{}[1].map{{|n|[2].map{{|m|v0+v63+n+m}}}};end;f",
            (0..64).map(|i| format!("v{i}={i};")).collect::<String>()
        ),
        format!(
            "module M;{}def self.value;N0+N63;end;end;M.value",
            (0..64).map(|i| format!("N{i}={i};")).collect::<String>()
        ),
        (0..64)
            .map(|i| format!("begin;1;rescue=>error{i};error{i};end;"))
            .collect(),
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
        "enum Collision;FooBar;Foo_Bar;end".into(),
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
fn compiler_diagnostic_messages_reserve_before_formatting_and_release_on_failure() {
    let message = "日本語".repeat(4096);
    let run = |work: &dyn Work| Error::syntax(work, 7, format_args!("expected {message}"));
    let baseline = Interrupt::new(usize::MAX, false);
    let error = run(&baseline);
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert_eq!(error.offset, Some(7));
    assert_eq!(error.message, format!("expected {message}"));
    let stats = baseline.context.borrow().stats();
    assert!(stats.retained_memory_bytes >= error.message.capacity());
    assert!(stats.peak_memory_bytes < message.len() * 2);
    assert!(baseline.largest_bytes.get() <= 4096);
    drop(error);
    assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
    for memory in [false, true] {
        for short in [0, 1] {
            let mut options = CallOptions::default();
            if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes - short);
            } else {
                options.limits.steps = Some(stats.steps - short as u64);
            }
            let mut context = CallContext::new(options);
            let error = run(&Meter(RefCell::new(&mut context)));
            if short == 0 {
                assert_eq!(error.kind, ErrorKind::Syntax);
            } else {
                let kind = if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                };
                assert_eq!(error.kind, kind);
                assert_eq!(context.checkpoint().unwrap_err().kind, kind);
            }
            drop(error);
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
    for deadline in [false, true] {
        for at in 0..baseline.visits.get() {
            let work = Interrupt::new(at, deadline);
            let error = run(&work);
            let kind = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert_eq!(error.kind, kind, "visit {at}");
            assert!(error.diagnostic.is_none());
            assert_eq!(work.context.borrow_mut().checkpoint().unwrap_err(), error);
            assert_eq!(work.context.borrow().stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn compiler_source_diagnostics_keep_exact_text_and_account_for_partial_construction() {
    let source = "head\n\t日本$";
    let filename: std::sync::Arc<[u8]> = ["pkg/日本語".as_bytes(), b"\n\xff.vibe"].concat().into();
    let run = |work: &dyn Work| {
        let error = Error::syntax(work, source.find('$').unwrap(), "expected expression");
        crate::source::parse_error(source, Some(&filename), error, work)
    };
    let expected = "  --> pkg/日本語\\n\\xff.vibe:2:4\n 2 | \t日本$\n   | \t  ^";
    let baseline = Interrupt::new(usize::MAX, false);
    let error = run(&baseline);
    assert_eq!(error.kind, ErrorKind::Syntax);
    let diagnostic = error.diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.position, crate::Position { line: 2, column: 4 });
    assert_eq!(diagnostic.code_frame, expected);
    assert!(diagnostic.frames.is_empty());
    assert!(std::sync::Arc::ptr_eq(
        diagnostic.filename.as_ref().unwrap(),
        &filename
    ));
    let stats = baseline.context.borrow().stats();
    let visits = baseline.visits.get();
    assert!(
        stats.retained_memory_bytes > error.message.capacity() + diagnostic.code_frame.capacity()
    );
    let budget = std::sync::Arc::downgrade(&baseline.context.borrow().identity());
    drop(baseline);
    assert!(budget.upgrade().is_some());
    drop(error);
    assert!(budget.upgrade().is_none());
    assert_eq!(run(&()).diagnostic.unwrap().code_frame, expected);
    for memory in [false, true] {
        for short in [0, 1] {
            let mut options = CallOptions::default();
            if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes - short);
            } else {
                options.limits.steps = Some(stats.steps - short as u64);
            }
            let mut context = CallContext::new(options);
            let error = run(&Meter(RefCell::new(&mut context)));
            if short == 0 {
                assert_eq!(error.diagnostic.as_ref().unwrap().code_frame, expected);
            } else {
                let kind = if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                };
                assert_eq!(error.kind, kind);
                assert!(error.diagnostic.is_none());
                assert_eq!(context.checkpoint().unwrap_err().kind, kind);
            }
            drop(error);
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
    for deadline in [false, true] {
        for at in 0..visits {
            let work = Interrupt::new(at, deadline);
            let error = run(&work);
            let kind = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert_eq!(error.kind, kind, "visit {at}");
            assert!(error.diagnostic.is_none());
            assert_eq!(work.context.borrow_mut().checkpoint().unwrap_err(), error);
            assert_eq!(work.context.borrow().stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn compiler_rejection_routes_retain_accounted_messages() {
    for source in [
        "@",
        "0xQ",
        "1__2",
        "\"\\xzz\"",
        "/x/z",
        "%w[unfinished",
        "$",
        "def f(",
        "if true; 1",
        "a=[];a.1",
        "a::1",
        "begin;1;rescue int;2;end",
        "class C;alias :'\\xff' :x;end",
        "enum int;A;end",
        "enum C;FooBar;Foo_Bar;end",
        "def C;1;end;module C;end",
        "1=2",
    ] {
        let baseline = Interrupt::new(usize::MAX, false);
        let error = crate::bytecode::compile_file(source, Vec::new(), &baseline).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        assert!(!error.message.is_empty());
        assert!(
            baseline.context.borrow().stats().retained_memory_bytes >= error.message.capacity()
        );
        assert!(error.retained_charge.is_some(), "{source}");
        let at = baseline.visits.get() - 1;
        drop(error);
        assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
        let cancelled = Interrupt::new(at, false);
        assert_eq!(
            crate::bytecode::compile_file(source, Vec::new(), &cancelled)
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
        assert_eq!(cancelled.context.borrow().stats().retained_memory_bytes, 0);
    }
}

#[test]
fn compiler_diagnostics_can_account_shared_and_untracked_input_errors() {
    for shared in [false, true] {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let mut input = if shared {
            Error::syntax(&work, 0, "expected expression")
        } else {
            Error::new(ErrorKind::Syntax, "expected expression")
        };
        input.offset = Some(0);
        let alias = shared.then(|| input.clone());
        let error = crate::source::parse_error("$", None, input, &work);
        assert_eq!(error.kind, ErrorKind::Syntax);
        assert_eq!(
            error.diagnostic.as_ref().unwrap().code_frame,
            "  --> line 1, column 1\n 1 | $\n   | ^"
        );
        let held = error.retained_charge.as_ref().unwrap().bytes();
        drop(alias);
        assert_eq!(context.stats().retained_memory_bytes, held);
        drop(error);
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn compiled_builtin_namespaces_retain_sorted_unique_members_without_the_budget() {
    let mut context = CallContext::new(CallOptions::default());
    let budget = std::sync::Arc::downgrade(&context.identity());
    let program = crate::bytecode::compile(
        "[Hash, Regexp, Regex, Time, Duration, JSON, Math]",
        Vec::new(),
        &Meter(RefCell::new(&mut context)),
    )
    .unwrap();
    assert_eq!(context.stats().retained_memory_bytes, 0);
    drop(context);
    assert!(budget.upgrade().is_none());
    let expected: &[(&str, &[&str])] = &[
        ("Hash", &["new"]),
        ("Regexp", &["escape", "last_match", "new", "quote", "union"]),
        (
            "Regex",
            &["escape", "match", "new", "replace", "replace_all", "union"],
        ),
        (
            "Time",
            &["at", "gm", "local", "mktime", "new", "now", "parse", "utc"],
        ),
        ("Duration", &["build", "parse"]),
        ("JSON", &["parse", "parse_as", "stringify"]),
        (
            "Math",
            &[
                "E", "PI", "acos", "asin", "atan", "atan2", "cbrt", "cos", "exp", "hypot", "log",
                "log10", "log2", "sin", "sqrt", "tan",
            ],
        ),
    ];
    assert_eq!(program.globals.len(), expected.len());
    for ((global, value), &(name, members)) in program.globals.iter().zip(expected) {
        assert_eq!(global.name(), name);
        let crate::value::Kind::Hash(hash) = &value.0 else {
            panic!("expected namespace {name}")
        };
        assert!(hash.object);
        assert_eq!(hash.depth, 1);
        let fields = &hash.buffer.data;
        assert_eq!(fields.len(), members.len());
        for ((key, value), member) in fields.iter().zip(members) {
            assert_eq!(key.as_bytes(), Some(member.as_bytes()), "{name}");
            match (name, *member, &value.0) {
                ("Math", "E", _) => assert_eq!(value.as_float(), Some(std::f64::consts::E)),
                ("Math", "PI", _) => assert_eq!(value.as_float(), Some(std::f64::consts::PI)),
                // The canonical spellings share Regexp's builtins, and their wording.
                ("Regex", "escape" | "new" | "union", crate::value::Kind::Builtin(builtin)) => {
                    assert_eq!(builtin.name(), format!("Regexp.{member}"))
                }
                (_, _, crate::value::Kind::Builtin(builtin)) => {
                    assert_eq!(builtin.name(), format!("{name}.{member}"))
                }
                _ => panic!("unexpected field {name}.{member}"),
            }
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

#[test]
fn interrupted_name_table_updates_leave_the_original_scope_intact() {
    let long_name = "名".repeat(4097);
    let names = [long_name.as_str(), "name_1", "name_2", "name_3"];
    let initial = || {
        let mut table = Table::new();
        for (i, name) in names.iter().enumerate() {
            table.insert(&(), Name::new(&(), name).unwrap(), i).unwrap();
        }
        table
    };
    for operation in ["insert", "replace", "remove", "lookup", "copy"] {
        let run = |table: &mut Table<usize>, work: &dyn Work| match operation {
            "copy" => table.copy(work).map(drop),
            "insert" => table
                .insert(work, Name::new(&(), "added").unwrap(), 4)
                .map(drop),
            "replace" => table
                .insert(work, Name::new(&(), &long_name).unwrap(), 99)
                .map(drop),
            "remove" => table.remove(work, &long_name).map(drop),
            "lookup" => table.get(work, &long_name).map(drop),
            _ => unreachable!(),
        };
        let mut table = initial();
        let baseline = Interrupt::new(usize::MAX, false);
        run(&mut table, &baseline).unwrap();
        drop(table);
        assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
        assert!(baseline.largest_bytes.get() <= 4096);
        if matches!(operation, "replace" | "remove" | "lookup") {
            assert!(baseline.byte_visits.get() >= 2 * long_name.len().div_ceil(4096));
        }
        let check_original = |table: &Table<usize>| {
            assert_eq!(table.get(&(), "added").unwrap(), None);
            for (i, name) in names.iter().enumerate() {
                assert_eq!(table.get(&(), name).unwrap(), Some(&i));
            }
        };
        let steps = baseline.context.borrow().stats().steps;
        for limit in [0, steps / 2, steps - 1, steps] {
            let work = Interrupt::new(usize::MAX, false);
            work.context.borrow_mut().options.limits.steps = Some(limit);
            let mut table = initial();
            let result = run(&mut table, &work);
            if limit == steps {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Steps);
                assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Steps);
                check_original(&table);
            }
            drop(table);
            assert_eq!(work.context.borrow().stats().retained_memory_bytes, 0);
        }
        let visits = baseline.visits.get();
        for at in [0, visits / 4, visits / 2, visits - 1] {
            for deadline in [false, true] {
                let interrupted = Interrupt::new(at, deadline);
                let mut table = initial();
                let kind = if deadline {
                    ErrorKind::Deadline
                } else {
                    ErrorKind::Cancelled
                };
                assert_eq!(run(&mut table, &interrupted).unwrap_err().kind, kind);
                assert_eq!(interrupted.checkpoint().unwrap_err().kind, kind);
                assert_eq!(
                    interrupted.context.borrow().stats().retained_memory_bytes,
                    0
                );
                check_original(&table);
            }
        }
    }
}

fn generation_sources() -> Vec<String> {
    let names = (0..64).map(|i| format!("v{i}")).collect::<Vec<_>>();
    let values = format!("[{}]", names.join(","));
    let mut nested = values.clone();
    for _ in 0..8 {
        nested = format!("{values};[0].map{{|n|{nested}}}");
    }
    let declarations = names
        .iter()
        .map(|name| format!("{name}=1;"))
        .collect::<String>();
    vec![
        format!("def unused;{declarations}{nested};end;0"),
        format!(
            "def unused(n);case n;{}else;nil;end;end;0",
            (0..96)
                .map(|i| format!("when {i},{},{};{i};", i + 100, i + 200))
                .collect::<String>()
        ),
        format!(
            "module Root;{}end;Root::M31.value",
            (0..32)
                .map(|i| format!("module M{i};N={i};def self.value;N;end;end;"))
                .collect::<String>()
        ),
        format!(
            "def unused(e);{}end;0",
            (0..64)
                .map(|i| format!("begin;raise 'failure';rescue=>e;v{i}=e;ensure;nil;end;"))
                .collect::<String>()
        ),
        format!(
            "[[[1]]].map{{|({}:array<int>)|nil}}",
            "binding".repeat(1024)
        ),
        format!(
            "enum States;{};end;0",
            (0..128)
                .map(|i| format!("Member{i}"))
                .collect::<Vec<_>>()
                .join(";")
        ),
    ]
}

#[test]
fn generation_storage_and_work_limits_cover_the_phase_after_parsing() {
    let mut generation_peaks = 0;
    for source in generation_sources() {
        let parsed = Interrupt::new(usize::MAX, false);
        drop(crate::syntax::parse(&source, &parsed).unwrap());
        let parse_steps = parsed.context.borrow().stats().steps;
        let parse_peak = parsed.context.borrow().stats().peak_memory_bytes;
        let parse_visits = parsed.visits.get();
        let run = |work: &dyn Work| crate::bytecode::compile_file(&source, Vec::new(), work);
        let baseline = Interrupt::new(usize::MAX, false);
        let budget = std::sync::Arc::downgrade(&baseline.context.borrow().identity());
        let program = run(&baseline).unwrap();
        let stats = baseline.context.borrow().stats();
        let visits = baseline.visits.get();
        assert_eq!(stats.retained_memory_bytes, 0);
        assert!(stats.steps > parse_steps && visits > parse_visits);
        drop(baseline);
        assert!(budget.upgrade().is_none());
        assert!(!program.functions.is_empty());
        drop(program);
        let peak = stats.peak_memory_bytes;
        let mut limits = vec![0, peak - 1, peak];
        if peak > parse_peak {
            generation_peaks += 1;
            limits.push(parse_peak);
        }
        for limit in limits {
            let mut options = CallOptions::default();
            options.limits.steps = None;
            options.limits.memory_bytes = Some(limit);
            let mut context = CallContext::new(options);
            let result = run(&Meter(RefCell::new(&mut context)));
            if limit == peak {
                drop(result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            }
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
        for limit in [parse_steps, stats.steps - 1, stats.steps] {
            let mut options = CallOptions::default();
            options.limits.steps = Some(limit);
            let mut context = CallContext::new(options);
            let result = run(&Meter(RefCell::new(&mut context)));
            if limit == stats.steps {
                drop(result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Steps);
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Steps);
            }
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
        for at in [
            parse_visits,
            parse_visits + (visits - parse_visits) / 2,
            visits - 1,
        ] {
            for deadline in [false, true] {
                let work = Interrupt::new(at, deadline);
                let kind = if deadline {
                    ErrorKind::Deadline
                } else {
                    ErrorKind::Cancelled
                };
                assert_eq!(run(&work).unwrap_err().kind, kind);
                assert_eq!(work.checkpoint().unwrap_err().kind, kind);
                assert_eq!(work.visits.get(), at + 1);
                assert_eq!(work.context.borrow().stats().retained_memory_bytes, 0);
            }
        }
    }
    assert!(
        generation_peaks > 0,
        "a code-generation allocation must exceed the parser peak"
    );
}

#[test]
fn compiler_bindings_preserve_shadowing_calls_and_internal_slot_names() {
    let run = |engine: crate::Engine, source: &str, expected: &str| {
        let result = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let json = crate::stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(
            json.value.as_bytes().unwrap(),
            expected.as_bytes(),
            "{source}"
        );
    };
    for (source, expected) in [
        (
            "def f(x: int) -> int;y=10;[1].map{|x|[2].map{|n|x+y+n}}.fetch(0).fetch(0);end;f(99)",
            "13",
        ),
        ("v=5;[1].map{[2].map{it+v}}", "[[7]]"),
        ("a=0;for n in [1,2];a+=n;next;end;a", "3"),
        (
            "module A;N=1;module B;N=2;end;end;module AB;N=4;end;[A.N,A::B.N,AB.N]",
            "[1,2,4]",
        ),
        (
            "[9223372036854775808,18446744073709551615]",
            "[9223372036854775808,18446744073709551615]",
        ),
        (
            "pairs: array<[int, int]> = [[1,2]];pairs.map{|(a:int,b:int)|a+b}",
            "[3]",
        ),
        // Locals named like functions leave calls with parentheses to them.
        (
            "def a -> int;3;end;def b -> int;5;end;a,b=[a()+b(),b()+a()];[a,b]",
            "[8,8]",
        ),
        // A rescue binding that shadows a parameter holds the error only in
        // its clause. The checker reads it as an assignment to the
        // parameter, which must therefore accept an error.
        (
            "def f(e: any) -> array<any>;s='';begin;raise 'failure';rescue=>e;s=e.as(error).message;end;[e,s];end;f(11)",
            "[11,\"failure\"]",
        ),
    ] {
        run(crate::Engine::new(), source, expected);
    }
}

#[test]
fn compiler_type_label_writes_reserve_storage_and_preserve_partial_failure() {
    let name = "Namespace::型".repeat(1024);
    let ty = crate::types::Type::named(name.clone());
    let baseline = Interrupt::new(usize::MAX, false);
    let mut text = Buffer::new();
    crate::shapes::format(&ty, &mut (&baseline as &dyn Work, &mut text)).unwrap();
    assert_eq!(&*text, name.as_bytes());
    let peak = baseline.context.borrow().stats().peak_memory_bytes;
    let visits = baseline.visits.get();
    drop(text);
    assert_eq!(baseline.context.borrow().stats().retained_memory_bytes, 0);
    for limit in [0, peak - 1, peak] {
        let mut options = CallOptions::default();
        options.limits.memory_bytes = Some(limit);
        let mut context = CallContext::new(options);
        let work = Meter(RefCell::new(&mut context));
        let mut text = Buffer::new();
        let result = crate::shapes::format(&ty, &mut (&work as &dyn Work, &mut text));
        if limit == peak {
            result.unwrap();
            assert_eq!(&*text, name.as_bytes());
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
            assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            assert!(name.as_bytes().starts_with(&text));
        }
        drop(text);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }
    for at in [0, visits / 2, visits - 1] {
        for deadline in [false, true] {
            let work = Interrupt::new(at, deadline);
            let mut text = Buffer::new();
            let kind = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert_eq!(
                crate::shapes::format(&ty, &mut (&work as &dyn Work, &mut text))
                    .unwrap_err()
                    .kind,
                kind
            );
            assert_eq!(work.checkpoint().unwrap_err().kind, kind);
            assert!(name.as_bytes().starts_with(&text));
            drop(text);
            assert_eq!(work.context.borrow().stats().retained_memory_bytes, 0);
        }
    }
}
