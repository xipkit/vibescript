use crate::{CallContext, CallOptions, Engine, ErrorKind, Limits, Value};

fn check(source: &str, clean: bool) {
    let script = Engine::new().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
}

#[test]
fn whole_scope_checks_unused_functions_and_methods() {
    for (source, clean) in [
        ("7;def unused(n:int)->int;n+1;end", true),
        ("7;def unused(n:string)->int;n;end", false),
        ("class C;private def unused->int;false;end;end;7", false),
        (
            "module M;def self.unused(n:int)->int;n+false;end;end;7",
            false,
        ),
        ("def unused(n:int=false);n;end;7", false),
    ] {
        check(source, clean);
    }
}

#[test]
fn declarations_accept_unknown_blocks_and_check_both_presence_paths() {
    for (source, clean) in [
        ("def run;yield 7;end", true),
        ("def run->int;yield;7;end", true),
        ("def run->int;if block_given?;yield;7;else;0;end;end", true),
        ("def run->int;if block_given?;false;else;7;end;end", false),
        ("def run->int;if block_given?;7;else;false;end;end", false),
        ("def run->int;yield;false;end", false),
        ("def run;yield;end;run()", false),
        ("def run;yield;end;def bad;run();end", false),
    ] {
        check(source, clean);
    }
}

#[test]
fn declaration_blocks_keep_lexical_yield_and_error_cleanup() {
    for (source, clean) in [
        ("def run;[1].each{yield};7;end", true),
        ("def once;yield;end;def run;once{yield};7;end", true),
        (
            "def once;yield;end;def run->int;once{yield};false;end",
            false,
        ),
        (
            "def run;begin;[1].each{yield};ensure;1+false;end;end",
            false,
        ),
        (
            "def run->int;begin;yield;rescue;false;else;7;end;end",
            false,
        ),
    ] {
        check(source, clean);
    }
}

#[test]
fn whole_scope_keeps_declaration_diagnostics_after_failing_top_level_and_initializers() {
    let source = "1+false\nmodule M\n2+false\nend\ndef later->int\nfalse\nend\n";
    let script = Engine::new().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    for line in [1, 3, 6] {
        assert!(
            report.diagnostics.iter().any(|d| d.position.line == line),
            "missing line {line}: {report:?}"
        );
    }
}

#[test]
fn declarations_keep_instances_from_each_failing_top_level_history() {
    // Error exits after different allocations reach the declarations as separate states.
    let source = "class Order\n  def initialize()\n  end\nend\nclass Holder\n  def initialize()\n  end\n  def check(u: Order)\n    raise \"x\"\n  end\nend\ndef takes_order(value: Order) -> Order\n  value\nend\ndef later -> int\n  false\nend\n[Holder][0].new.check(Order.new)";
    let script = Engine::new().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert_eq!(report.diagnostics[0].position.line, 16, "{report:?}");
    let error = script.run(CallOptions::default()).unwrap_err();
    assert_eq!(error.message, "x");
}

#[test]
fn whole_scope_keeps_effective_methods_and_constructor_field_contracts() {
    for (source, clean) in [
        ("class C;def f->int;false;end;def f->int;7;end;end", true),
        (
            "class C;property n:int;def initialize(@n: int);end;def value->int;@n;end;end",
            true,
        ),
        (
            "class C;property n:int;def initialize(n:int);@n=n;end;def value->int;@n;end;end",
            true,
        ),
        ("class C;property n:int;def value->int;@n;end;end", false),
    ] {
        check(source, clean);
    }
}

#[test]
fn unknown_blocks_preserve_local_values_and_possible_external_writes() {
    for (source, clean) in [
        ("def run->int;n=7;yield;n;end", true),
        (
            "module M;@@n='bad';def self.n;@@n;end;end;def run->int;yield;M.n;end",
            false,
        ),
        (
            "class C;property n;def run->int;@n='bad';yield;@n;end;end",
            false,
        ),
        ("def run->int;n=7;[1].each{yield;n=8};n;end", true),
        (
            "def run->int;begin;n=7;yield;n='bad';rescue;n='bad';ensure;n+false;end;7;end",
            false,
        ),
    ] {
        check(source, clean);
    }
}

#[test]
fn whole_scope_uses_top_level_namespace_state_and_checks_later_bodies() {
    for (source, clean) in [
        (
            "x=7;module M;Result=x;def self.run->int;Result;end;end",
            true,
        ),
        (
            "x='bad';module M;Result=x;def self.run->int;Result;end;end",
            false,
        ),
        (
            "module M;@@n=7;def self.value->int;@@n;end;end;M[:n]=false",
            false,
        ),
        (
            "module M;Good=7;module N;Good=9;def self.run->int;Good;end;end;end",
            true,
        ),
        (
            "def choose(flag:bool)->int;if flag;false;else;7;end;end;choose(false)",
            false,
        ),
    ] {
        check(source, clean);
    }
    let source = "module A\n1+false\nend\nmodule B\n2+false\nend\ndef later->int\nfalse\nend\n";
    let script = Engine::new().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    for line in [2, 5, 8] {
        assert!(
            report.diagnostics.iter().any(|d| d.position.line == line),
            "missing line {line}: {report:?}"
        );
    }
}

#[test]
fn constructor_fields_follow_branches_helpers_and_cleanup() {
    for (initializer, clean) in [
        ("def initialize(n:bool);if n;@n=7;else;@n=8;end;end", true),
        ("def initialize(n:bool);if n;@n=7;end;end", false),
        ("def initialize(n:bool);if n;return;end;@n=7;end", false),
        ("def initialize;begin;return;ensure;@n=7;end;end", true),
        (
            "def initialize;begin;raise 'stop';rescue;nil;end;end",
            false,
        ),
        (
            "def initialize;assign(7);end;def assign(n:int);@n=n;end",
            true,
        ),
        ("def initialize;[7].each{|n|@n=n};end", true),
        ("def initialize;yield;@n=7;end", false),
        ("def initialize;@n=7;yield;end", true),
    ] {
        check(
            &format!("class C;property n:int;{initializer};def value->int;@n;end;end"),
            clean,
        );
    }
}

#[test]
fn constructor_facts_preserve_runtime_witnesses_and_symbolic_parameters() {
    let script = Engine::new().compile("class C;property n:int;def initialize(set:bool);if set;@n=7;end;end;def value->int;@n;end;end;def read(c:C)->int;c.value;end;def witness(set:bool);read(C.new(set));end").unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert_eq!(
        script
            .call("witness", &[Value::boolean(true)], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(
        script
            .call("witness", &[Value::boolean(false)], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    check(
        "class C;property n:int;def initialize(@n: int);end;end;def read(c:C)->int;c.n;end",
        true,
    );
    check(
        "class C;property n:int;def initialize(@n: int);end;end;def read(c:array<C>)->array<int>;c.map{|v|v.n};end",
        true,
    );
    check(
        "class C;property n:int;def initialize(other:C);@n=7;end;def value->int;@n;end;end",
        true,
    );
}

#[test]
fn declaration_block_control_preserves_lexical_homes_and_pending_writes() {
    for (source, clean) in [
        (
            "def once;yield;end;def run->int;begin;once{once{yield}};ensure;return false;end;7;end",
            false,
        ),
        ("def run;while true;yield;break;end;7;end", true),
        ("def run->int;[1].each{yield};false;end", false),
        (
            "def run->array<int>;a=[1];a[-1]+=begin;yield;2;end;a;end",
            true,
        ),
        (
            "class C;property items;def run->array<int>;@items=[1];@items[-1]+=begin;yield;false;end;@items;end;end",
            false,
        ),
        ("def need;yield;end;def run;need;end", false),
    ] {
        check(source, clean);
    }
}

#[test]
fn whole_scope_metering_spans_all_declarations_and_releases_temporary_state() {
    for source in [
        "1+false;module M;2+false;end;def bad->int;false;end;def block;yield;end",
        "class C;property n:int;def initialize(@n: int);end;def value->int;if block_given?;yield;end;@n;end;end;def read(c:C)->int;c.n;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions::default();
        let mut ctx = CallContext::new(options.clone());
        let report = super::whole::check(&mut ctx, &script, &options).unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        let stats = ctx.stats();
        drop(report);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            for sample in [0, 1, 8, 15, 16] {
                let options = CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(stats.steps * sample / 16),
                        memory_bytes: (kind == ErrorKind::Memory)
                            .then_some(stats.peak_memory_bytes * sample as usize / 16),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                };
                let mut ctx = CallContext::new(options.clone());
                let report = super::whole::check(&mut ctx, &script, &options);
                if sample == 16 {
                    drop(report.unwrap());
                } else {
                    assert_eq!(report.unwrap_err().kind, kind);
                    assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
                }
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
        for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut options = CallOptions::default();
            if kind == ErrorKind::Cancelled {
                options.cancellation.cancel();
            } else {
                options.deadline = Some(std::time::Instant::now());
            }
            let mut ctx = CallContext::new(options.clone());
            assert_eq!(
                super::whole::check(&mut ctx, &script, &options)
                    .unwrap_err()
                    .kind,
                kind
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn declaration_state_uses_successful_top_level_exits_when_available() {
    let mut engine = Engine::new();
    engine.register("input", |_, _| Ok(Value::int(7)));
    let script = engine
        .compile("module M;K=input();def self.value->int;K;end;end;M.value")
        .unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        script
            .call("__main__", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    let script = engine
        .compile("input();module M;K=7;def self.value->int;K;end;end")
        .unwrap();
    assert!(script.check(&CallOptions::default()).unwrap().is_clean());
}

#[test]
fn constructor_class_inputs_do_not_invent_uninitialized_fields() {
    for source in [
        "class C;property n:int;def initialize(other:C?=nil);if other;@n=other.n;else;@n=7;end;end;def value->int;@n;end;end;def witness;a=C.new;C.new(a).value;end",
        "class D;property n:int;def initialize;@n=7;end;end;class C;property n:int;def initialize(other:D);@n=other.n;end;def value->int;@n;end;end;def witness;C.new(D.new).value;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script.check(&CallOptions::default()).unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert_eq!(
            script
                .call("witness", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
}
