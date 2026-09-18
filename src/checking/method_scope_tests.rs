use super::entry;
use crate::{CallContext, CallOptions, Engine, ErrorKind, Limits, Value};

fn check(source: &str, name: &str, clean: bool) {
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_function(name, &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
}

#[test]
fn general_method_entries_check_declared_domains_without_constructing_receivers() {
    for (source, name, clean) in [
        (
            "class C;def answer(x:int)->int;x+1;end;end",
            "C#answer",
            true,
        ),
        (
            "class C;def answer(x:string)->int;x;end;end",
            "C#answer",
            false,
        ),
        (
            "module M;def self.answer(x:int)->int;x+1;end;end",
            "M.answer",
            true,
        ),
        (
            "class C;property n:int;def write(n:int)->int;@n=n;@n;end;end",
            "C#write",
            true,
        ),
        (
            "class C;property n:int;def corrupt;@n=false;end;end",
            "C#corrupt",
            false,
        ),
        (
            "class C;property n;def corrupt(other:C)->int;@n=7;other.n='bad';@n;end;end",
            "C#corrupt",
            false,
        ),
        (
            "class C;def initialize(@n:int)->int;false;end;end",
            "C.new",
            true,
        ),
    ] {
        check(source, name, clean);
    }
}

#[test]
fn general_class_parameters_keep_method_results_and_argument_contracts() {
    for (source, clean) in [
        (
            "class C;def answer(x:int)->int;x+1;end;end;def run(value:C)->int;value.answer(7);end",
            true,
        ),
        (
            "class C;def answer(x:int)->int;x+1;end;end;def run(value:C)->int;value.answer(false);end",
            false,
        ),
        (
            "class C;def answer;false;end;end;def run(value:C)->int;value.answer;end",
            false,
        ),
    ] {
        check(source, "run", clean);
    }
}

#[test]
fn symbolic_receivers_preserve_possible_aliases_and_definite_self_writes() {
    for (source, clean) in [
        (
            "class C;property n;end;def run(a:C,b:C)->int;a.n=7;b.n='bad';a.n;end",
            false,
        ),
        (
            "class C;property n;end;def run(a:C)->int;a.n='bad';a.n=7;a.n;end",
            true,
        ),
        (
            "class C;end;def run(a:C,b:C)->int;if a==b;false;else;7;end;end",
            false,
        ),
        (
            "class C;end;def run(a:C)->int;if a==a;7;else;false;end;end",
            true,
        ),
    ] {
        check(source, "run", clean);
    }
}

#[test]
fn method_property_domains_reject_known_nested_writes_and_keep_unset_fields() {
    for (source, clean) in [
        ("class C;getter n:int;def run;@n+false;end;end", false),
        (
            "class C;property items:array<int>;def run;@items=[1];@items.push('bad');end;end",
            false,
        ),
        (
            "class C;property items:array<int>;def run->array<int>;@items=[1];@items.push(2);@items;end;end",
            true,
        ),
        ("class C;getter n:int;def run->int;@n;end;end", false),
        (
            "class C;getter n:int;def n=(x);@n=x;end;def run;@n=false;end;end",
            true,
        ),
    ] {
        check(source, "C#run", clean);
    }
}

#[test]
fn class_domains_survive_defaults_unions_shapes_and_collection_elements() {
    for (source, clean) in [
        (
            "class C;property n;end;def run(a:C=C.new)->int;a.n=7;a.n;end",
            true,
        ),
        (
            "class C;def answer;7;end;end;def run(a:C?)->int;if a.nil?;0;else;a.answer;end;end",
            true,
        ),
        (
            "class C;def answer;7;end;end;def run(a:array<C>)->array<int>;a.map{|x|x.answer};end",
            true,
        ),
        (
            "class C;def answer;false;end;end;def run(a:array<C>)->array<int>;a.map{|x|x.answer};end",
            false,
        ),
        (
            "class C;property n;end;def run(a:{left:C,right:C})->int;a[:left].n=7;a[:right].n='bad';a[:left].n;end",
            false,
        ),
        (
            "class C;property n;end;def run(a:array<C>)->array<int>;a.map{|x|x.n=7;a.each{|y|y.n='bad'};x.n};end",
            false,
        ),
    ] {
        check(source, "run", clean);
    }
}

#[test]
fn method_entries_keep_recursive_calls_blocks_and_error_exits() {
    for (source, name, clean) in [
        (
            "class C;def run(n:int)->int;if n>0;run(n-1);else;7;end;end;end",
            "C#run",
            true,
        ),
        (
            "class C;def run(n:int)->int;if n>0;run(false);else;7;end;end;end",
            "C#run",
            false,
        ),
        (
            "class C;property n;def run->int;@n=1;[2].each{|x|@n+=x};@n;end;end",
            "C#run",
            true,
        ),
        (
            "class C;property n;def bad;@n='bad';raise 'stop';end;def run->int;@n=7;begin;bad;rescue;nil;end;@n;end;end",
            "C#run",
            false,
        ),
        (
            "class C;def initialize(@n:int)->int;false;end;end",
            "C#initialize",
            false,
        ),
        (
            "class C;property n;def run->int;@n=7;begin;return @n;ensure;@n='bad';end;end;end",
            "C#run",
            true,
        ),
    ] {
        check(source, name, clean);
    }
}

#[test]
fn general_method_selection_uses_effective_definitions_and_namespace_identity() {
    for (source, name, clean) in [
        (
            "module M;module N;def self.value->int;7;end;end;end",
            "M::N.value",
            true,
        ),
        (
            "class C;private def value->int;false;end;end",
            "C#value",
            false,
        ),
        (
            "class C;def value->int;false;end;def value->int;7;end;end",
            "C#value",
            true,
        ),
        (
            "class C;def self.value->int;false;end;def value->int;7;end;end",
            "C.value",
            false,
        ),
        (
            "class C;def self.value->int;false;end;def value->int;7;end;end",
            "C#value",
            true,
        ),
        (
            "enum E;Yes;end;class C;property n:E;def value->E;@n=:yes;@n;end;end",
            "C#value",
            true,
        ),
    ] {
        check(source, name, clean);
    }
    for name in [
        "Missing#value",
        "C#nope",
        "C.value",
        "C#<initialize>",
        "M.new",
    ] {
        let script = Engine::new()
            .compile("module M;end;class C;def value;7;end;end")
            .unwrap();
        assert_eq!(
            script
                .check_function(name, &CallOptions::default())
                .unwrap_err()
                .kind,
            crate::ErrorKind::Name
        );
    }
}

#[test]
fn general_identity_helpers_and_nil_overrides_keep_possible_paths() {
    for (source, clean) in [
        (
            "class C;end;def run(a:C,b:C)->int;if a.equal?(b);false;else;7;end;end",
            false,
        ),
        (
            "class C;end;def run(a:C)->int;if a.eql?(a);7;else;false;end;end",
            true,
        ),
        (
            "class C;end;class D;end;def run(a:C,b:D)->int;if a==b;false;else;7;end;end",
            true,
        ),
        (
            "class C;property n;end;def run(a:C)->int;b=a.dup;a.n=7;b.n='bad';a.n;end",
            false,
        ),
        (
            "class C;def nil?;true;end;end;def run(a:C?)->int;if a.nil?;false;else;7;end;end",
            false,
        ),
        (
            "class C;def nil?;false;end;def answer;7;end;end;def run(a:C?)->int;if a.nil?;0;else;a.answer;end;end",
            true,
        ),
        (
            "class C;def self.nil?;true;end;end;def run->int;if C.nil?;false;else;7;end;end",
            false,
        ),
    ] {
        check(source, "run", clean);
    }
}

#[test]
fn fresh_instances_remain_distinct_from_general_inputs() {
    for source in [
        "class C;property n;end;def run(a:C)->int;a.n=7;b=C.new;b.n='bad';a.n;end",
        "class C;property n;end;def run(a:C)->int;b=C.new;b.n=7;a.n='bad';b.n;end",
        "class C;end;def run(a:C)->int;b=C.new;if a==b;false;else;7;end;end",
    ] {
        check(source, "run", true);
    }
}

#[test]
fn field_targets_preserve_reentrant_alias_writes_and_replacements() {
    for (source, clean) in [
        (
            "class C;property items;end;def append(c:C);c.items.push(2);'bad';end;def run(c:C)->array<int>;c.items=[1];c.items[-1]=append(c);c.items;end",
            true,
        ),
        (
            "class C;property items;end;def run(c:C)->array<int>;c.items=[1];c.items[0]=(begin;c.items=[7];'bad';end);c.items;end",
            true,
        ),
        (
            "class C;property items;end;def replace(c:C);c.items=['bad'];7;end;def run(a:C,b:C)->array<int>;a.items=[1];a.items[0]=replace(b);a.items;end",
            false,
        ),
    ] {
        check(source, "run", clean);
    }
    for (source, clean) in [
        (
            "class C;property items;def append;@items.push([2]);'bad';end;def run->array<array<int>>;@items=[[1]];@items[-1].push(append());@items;end;end",
            false,
        ),
        (
            "class C;property items;def replace;@items=[[7]];'bad';end;def run->array<array<int>>;@items=[[1]];@items[0].push(replace());@items;end;end",
            true,
        ),
    ] {
        check(source, "C#run", clean);
    }
}

#[test]
fn runtime_witnesses_confirm_general_alias_contradictions() {
    let script = Engine::new().compile("class C;property n;end;def run(a:C,b:C)->int;a.n=7;b.n='bad';a.n;end;def witness(same);a=C.new;b=if same;a;else;C.new;end;run(a,b);end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(
        script
            .call("witness", &[Value::boolean(true)], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(
        script
            .call("witness", &[Value::boolean(false)], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    let script = Engine::new().compile("class C;property n;end;def run(a:array<C>)->array<int>;a.map{|x|x.n=7;a.each{|y|y.n='bad'};x.n};end;def witness;run([C.new]);end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(
        script
            .call("witness", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
}

#[test]
fn method_and_object_domains_are_metered_interruptible_and_reclaimed() {
    for (source, name) in [
        (
            "class C;property n:int;def run(n:int)->int;@n=n;[2].each{|x|@n+=x};@n;end;end",
            "C#run",
        ),
        (
            "class C;property n;end;def run(a:array<C>)->array<int>;a.map{|x|x.n=7;a.each{|y|y.n='bad'};x.n};end",
            "run",
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions::default();
        let mut ctx = CallContext::new(options.clone());
        let checked = entry::check_function(&mut ctx, &script, name, &options).unwrap();
        assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
        let stats = ctx.stats();
        drop(checked);
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
                let checked = entry::check_function(&mut ctx, &script, name, &options);
                if sample == 16 {
                    drop(checked.unwrap());
                } else {
                    assert_eq!(checked.unwrap_err().kind, kind);
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
                entry::check_function(&mut ctx, &script, name, &options)
                    .unwrap_err()
                    .kind,
                kind
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
