use super::*;

#[test]
fn constructors_and_instance_methods_keep_identity_and_field_snapshots() {
    for call in ["C.new(3)", "(C.new)(3)"] {
        run(
            &format!(
                "class C;property count;def initialize(@count);return 99;end;def bump(n=1);@count+=n;end;end;def run;a={call};b=a;b.bump(2);[a.count,b.count,a==b,a.class==C];end"
            ),
            "[5, 5, true, true]",
        );
    }
    run(
        "class C;property n;def initialize(@n);end;end;def run;a=C.new(1);b=C.new(2);a.n=3;[a.n,b.n,a==b];end",
        "[3, 2, false]",
    );
    run(
        "class C;def initialize(v);@items=[v];end;def append(v);@items.push(v);end;def items;@items;end;end;def run;c=C.new(1);before=c.items;c.append(2);c.items.push(99);[before,c.items];end",
        "[[1], [1, 2]]",
    );
    run(
        "class C;def initialize->int;return \"ignored\";end;def value;7;end;end;def run;C.new.value;end",
        "7",
    );
    run(
        "class C;property n;end;def run;count=0;c=C.new(1,other:2){count+=1};[c.n,count];end",
        "[nil, 0]",
    );
}

#[test]
fn instance_blocks_retain_lexical_receivers_through_other_methods() {
    for body in ["[1,2].each{|n|@count+=n}", "Other.run{|n|@count+=n}"] {
        run(
            &format!(
                "module Other;def self.run;yield(1);yield(2);end;end;class C;property count;def initialize(@count);end;def work;{body};self;end;end;def run;c=C.new(4);c.work.count;end"
            ),
            "7",
        );
    }
    run(
        "class C;property count;def initialize(@count);end;def each;yield(3);end;def work(other);other.each{|n|@count+=n};end;end;def run;a=C.new(1);b=C.new(10);a.work(b);[a.count,b.count];end",
        "[4, 10]",
    );
    run(
        "class C;property count;def initialize(@count);end;def each;[1].each{yield(2)};end;def work(other);other.each{|n|[n].each{|i|@count+=i}};end;end;def run;a=C.new(1);b=C.new(10);a.work(b);[a.count,b.count];end",
        "[3, 10]",
    );
}

#[test]
fn instance_property_types_guard_all_field_stores() {
    for mutation in [
        "@items=[\"bad\"]",
        "@items.push(\"bad\")",
        "@items[0]=\"bad\"",
        "@items.fill{|x|\"bad\"}",
    ] {
        let source = format!(
            "class C;property items:array<int>;def initialize;@items=[1];end;def bad;{mutation};end;end;def run;c=C.new;begin;c.bad;rescue RuntimeError;nil;end;c.items;end"
        );
        let script = Engine::legacy_unchecked().compile(&source).unwrap();
        witness(&script, &[], &CallOptions::default(), "[1]", true);
    }
    let script = Engine::legacy_unchecked().compile("class C;getter n:int;def initialize;@n=1;end;def replace(@n);end;end;def run;c=C.new;begin;c.replace(\"bad\");rescue RuntimeError;nil;end;c.n;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "1", true);
    run(
        "enum E;a;b;end;class C;property e:E;def initialize;@e=:a;end;end;def run;c=C.new;c.e=:b;c.e;end",
        "E::b",
    );
}

#[test]
fn instance_addresses_survive_reentry_and_detach_replaced_collections() {
    run(
        "class C;getter items;def initialize;@items=[1,2];end;def append;@items.push(4);7;end;def work;@items[-1]+=append();@items;end;end;def run;C.new.work;end",
        "[1, 9, 4]",
    );
    run(
        "class C;getter items;def initialize;@items=[1];end;def replace;@items=[9];7;end;def work;@items[0]+=replace();@items;end;end;def run;C.new.work;end",
        "[9]",
    );
    run(
        "class C;getter items;def initialize;@items=[1];end;def work;@items[0]+=(begin;C.new;7;end);@items;end;end;def run;C.new.work;end",
        "[8]",
    );
}

#[test]
fn instance_aliases_and_cycles_survive_typed_call_boundaries() {
    run(
        "enum E;a;b;end;class C;property link,n;def initialize(@n);end;end;def echo(c:C)->C;c;end;def run;a=C.new(1);b=C.new(2);a.link=b;b.link=a;c=echo(a);c.n=3;[b.link.n,c.link.n,a==c];end",
        "[3, 2, true]",
    );
    run(
        "class C;property n;end;def touch(xs:array<C>)->array<C>;xs[0].n=7;xs;end;def run;a=C.new;b=touch([a]);[a.n,b[0].n];end",
        "[7, 7]",
    );
    run(
        "class C;end;def run;c=C.new;c.extra=[1];c.extra.push(2);c.extra;end",
        "[1]",
    );
}

#[test]
fn instance_helpers_preserve_identity_and_user_method_priority() {
    run(
        "class C;property n;end;def run;a=C.new;b=a.dup;c=(a.clone)();b.n=7;[a.n,a==b,a==c,a.equal?(b),a.eql?(c),a.nil?,a.frozen?,a.itself==a];end",
        "[7, true, true, true, true, false, true, true]",
    );
    for method in ["nil?", "dup", "clear", "map", "push", "equal?"] {
        run(
            &format!(
                "class C;def {method}(x=7);yield(x);end;end;def run;C.new.{method}(3){{|n|n+1}};end"
            ),
            "4",
        );
    }
    let script = Engine::legacy_unchecked()
        .compile("class C;end;def run;begin;C.new.dup(3);rescue RuntimeError;7;end;end")
        .unwrap();
    witness(&script, &[], &CallOptions::default(), "7", true);
}

#[test]
fn instance_lookup_visibility_and_setters_match_runtime() {
    run(
        "class C;private def hidden;7;end;protected def visible;hidden;end;public def answer;self.visible;end;end;def run;C.new.answer;end",
        "7",
    );
    for call in [
        "c.hidden",
        "c.visible",
        "c.answer",
        "c::anything",
        "c.readonly=3",
    ] {
        let script = Engine::legacy_unchecked().compile(&format!("class C;private def hidden;7;end;protected def visible;7;end;public def answer;self.hidden;end;getter readonly;end;def run;c=C.new;begin;{call};rescue RuntimeError;9;end;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "9", true);
    }
    run(
        "class C;getter n;def initialize(@n);end;def n=(n:int);@n=n+1;99;end;end;def run;a=C.new(1);b=C.new(2);c=a;x=begin;c.n=(begin;c=b;7;end);end;[a.n,b.n,x];end",
        "[1, 8, 7]",
    );
    run(
        "class C;getter n;def initialize(@n);end;def n=(n);@n=n+1;99;end;end;def run;a=C.new(1);b=C.new(2);c=a;x=begin;c.n+=(begin;c=b;7;end);end;[a.n,b.n,x];end",
        "[9, 2, 8]",
    );
    let script = Engine::legacy_unchecked().compile("class C;protected def secret;1;end;def self.test(c);c.secret;end;end;def run;begin;C.test(C.new);rescue RuntimeError;9;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "9", true);
}

#[test]
fn constructor_defaults_errors_and_ensure_keep_observable_state() {
    run(
        "class C;property n;def initialize(@n: int=2,scale:3);@n*=scale;ensure;@n+=1;end;end;def run;[C.new.n,C.new(4,scale:2).n];end",
        "[7, 9]",
    );
    run(
        "class C;property n;def initialize;@@last=self;@n=1;raise \"stop\";ensure;@n+=2;end;def self.last;@@last;end;end;def run;begin;C.new;rescue RuntimeError;nil;end;C.last.n;end",
        "3",
    );
    let script = Engine::legacy_unchecked().compile("class C;def initialize(n:int);@@n=n;end;end;def run;begin;C.new(\"bad\");rescue RuntimeError;7;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "7", true);
    run(
        "class C;end;def run;n=0;c=C.new(begin;n+=1;end,extra:(begin;n+=2;end)){n+=99};n;end",
        "3",
    );
}

#[test]
fn instance_fields_keep_global_effects_and_union_dispatch() {
    for flag in [false, true] {
        let mut engine = Engine::legacy_unchecked();
        engine.register("choose", move |_, _| Ok(Value::boolean(flag)));
        let script = engine.compile("class A;property n;def initialize;@n=1;end;def bump;@n+=2;end;end;class B;property n;def initialize;@n=10;end;def bump;@n+=3;end;end;def run;a=A.new;b=B.new;c=if choose();a;else;b;end;c.bump;[a.n,b.n];end").unwrap();
        witness(
            &script,
            &[],
            &CallOptions::default(),
            if flag { "[3, 10]" } else { "[1, 13]" },
            false,
        );
    }
    run(
        "class C;@@count=0;property n;def initialize;@@count+=1;@n=@@count;end;def self.count;@@count;end;end;def run;a=C.new;b=C.new;[a.n,b.n,C.count];end",
        "[1, 2, 2]",
    );
}

#[test]
fn ignored_host_transfers_from_instances_keep_constructor_and_method_homes() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        let _ = call.call_block(&[]);
        let _ = call.call_block(&[]);
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: true,
    })
    .unwrap();
    let driver = Value::object(vec![(b"visit".to_vec(), method.value())]);
    for (transfer, expected) in [("break 7", "[7, 1]"), ("return 9", "[9, 1]")] {
        let script = Engine::legacy_unchecked().compile(&format!("class C;property n;def initialize;@n=0;end;def go(driver);driver.visit{{@n+=1;{transfer}}};end;end;def run(driver);c=C.new;[c.go(driver),c.n];end")).unwrap();
        witness(
            &script,
            std::slice::from_ref(&driver),
            &CallOptions::default(),
            expected,
            false,
        );
    }
    let script = Engine::legacy_unchecked().compile("class C;property n;def initialize(driver);@n=0;driver.visit{@n+=1;return 9};@n=99;ensure;@n+=2;end;end;def run(driver);C.new(driver).n;end").unwrap();
    witness(&script, &[driver], &CallOptions::default(), "3", false);
}

#[test]
fn instance_analysis_summarizes_loop_allocations() {
    let source = "class C;end;def run(n:int);for i in 1..n;C.new;end;end";
    let script = Engine::legacy_unchecked().compile(source).unwrap();
    let report = script
        .check_call("run", &[Value::int(5)], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
    assert!(
        script
            .call("run", &[Value::int(5)], CallOptions::default())
            .is_ok()
    );
}

#[test]
fn instance_heaps_and_property_checks_are_metered_interruptible_and_released() {
    for source in [
        "class C;property n:int;def initialize(@n: int);end;def bump;@n+=1;end;end;def run;c=C.new(1);c.bump;c.n;end",
        "class C;getter xs:array<int>;def initialize;@xs=[1,2];end;def bump;@xs[-1]+=(begin;@xs.push(4);7;end);end;end;def run;C.new.bump;end",
        "class C;property link;end;def run;a=C.new;b=C.new;a.link=b;b.link=a;a.link==b;end",
    ] {
        metered(source);
    }
}
