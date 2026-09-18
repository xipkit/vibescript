use super::*;

#[test]
fn binary_operators_use_method_results_and_instance_effects() {
    for operator in [
        "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=", "<=>",
    ] {
        run(
            &format!(
                "class C;property n;def initialize;@n=1;end;def {operator}(x:int)->int;@n+=x;@n;end;end;def run;c=C.new;v=c {operator} 2;[v,c.n];end"
            ),
            "[3, 3]",
        );
    }
    run(
        "class C;property n;def initialize;@n=1;end;def +(x);@n+=x;self;end;end;def run;c=C.new;alias=c;c+=2;[c==alias,c.n];end",
        "[true, 3]",
    );
    run(
        "class C;end;def run;c=C.new;[c==c,c != c,c==C.new];end",
        "[true, false, false]",
    );
}

#[test]
fn inequality_uses_exact_override_before_negating_equality_truthiness() {
    for (result, expected) in [
        ("nil", "true"),
        ("false", "true"),
        ("0", "false"),
        ("[]", "false"),
        ("true", "false"),
    ] {
        run(
            &format!("class C;def ==(x);{result};end;end;def run;c=C.new;c != c;end"),
            expected,
        );
    }
    run(
        "class C;def ==(x);true;end;def !=(x);7;end;end;def run;c=C.new;c != c;end",
        "7",
    );
    run(
        "class C;def !=(x);7;end;def ==(x);true;end;end;def run;c=C.new;c != c;end",
        "7",
    );
}

#[test]
fn operators_respect_visibility_and_argument_return_contracts() {
    for expression in ["c[0]", "c<<1"] {
        let script = Engine::new().compile(&format!("class C;end;def run;c=C.new;begin;{expression};rescue TypeError;1;rescue RuntimeError;2;end;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "2", true);
    }
    for (definition, expression, expected) in [
        ("private;def +(x);7;end", "c+1", "99"),
        ("protected;def +(x);7;end", "c+1", "99"),
        ("private;def ==(x);false;end", "c != c", "99"),
        ("def +(x:int);x;end", "c+false", "99"),
        ("def +(x)->int;false;end", "c+1", "99"),
        ("def [](i:int);7;end", "c[false]", "99"),
        ("def [](i);7;end", "c[1,2]", "99"),
        ("", "c[0]", "99"),
        ("", "begin;c[0]=1;end", "99"),
        ("", "c<<1", "99"),
    ] {
        let source = format!(
            "class C;{definition};end;def run;c=C.new;begin;{expression};rescue;99;end;end"
        );
        let script = Engine::new().compile(&source).unwrap();
        witness(&script, &[], &CallOptions::default(), expected, true);
    }
    run(
        "class C;def use(other);other+1;end;protected;def +(x);7;end;end;def run;C.new.use(C.new);end",
        "7",
    );
    let script = Engine::new().compile("class C;def self.use(other);other+1;end;protected;def +(x);7;end;end;def run;begin;C.use(C.new);rescue;99;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
}

#[test]
fn indexed_operators_bind_multiple_arguments_and_preserve_assignment_values() {
    run(
        "class C;property n;def initialize;@n=1;end;def [](a,b=2);@n+a+b;end;def []=(a,b,value);@n=a+b+value;99;end;end;def run;c=C.new;a=c[3];b=c[3,4];v=begin;c[2,3]=7;end;[a,b,v,c.n];end",
        "[6, 8, 7, 12]",
    );
    run(
        "class C;property n;def initialize;@n=3;end;def [](i);@n;end;def []=(i,value);@n=value*2;99;end;end;def run;c=C.new;v=begin;c[0]+=4;end;[v,c.n];end",
        "[7, 14]",
    );
    run(
        "class C;property n;def initialize;@n=nil;end;def [](i);@n;end;def []=(i,value);@n=value;99;end;end;def run;c=C.new;c[0]||=7;c[0]||=9;c[0]&&=11;[c[0],c.n];end",
        "[11, 11]",
    );
    run(
        "class C;def [](*xs);xs.sum;end;end;def run;C.new[1,2,3];end",
        "6",
    );
}

#[test]
fn indexed_assignment_and_compound_reads_preserve_evaluation_order() {
    run(
        "class C;property trace;def initialize;@trace=[];end;def mark(n);@trace.push(n);n;end;def [](i);@trace.push(3);10;end;def []=(i,v);@trace.push(5);end;end;def run;c=C.new;c[c.mark(2)]=c.mark(1);a=c.trace;c[c.mark(2)]+=c.mark(4);[a,c.trace];end",
        "[[1, 2, 5], [1, 2, 5, 2, 3, 4, 5]]",
    );
    run(
        "class C;property n;def initialize;@n=1;end;def [](i);@n;end;def []=(i,v);@n=v;end;end;def run;a=C.new;b=C.new;c=a;c[0]+=(begin;c=b;7;end);[a.n,b.n,c==b];end",
        "[8, 1, true]",
    );
}

#[test]
fn indexed_method_results_keep_collection_copies_and_object_identity() {
    run(
        "class C;getter xs;def initialize;@xs=[1,2];end;def [](i);@xs;end;end;def run;c=C.new;c[0].push(3);c[0][0]=9;c.xs;end",
        "[1, 2]",
    );
    run(
        "class Leaf;property n;def initialize;@n=1;end;end;class C;getter leaf;def initialize;@leaf=Leaf.new;end;def [](i);@leaf;end;end;def run;c=C.new;c[0].n=7;c.leaf.n;end",
        "7",
    );
    run(
        "class C;getter xs;def initialize;@xs=[[1]];end;def [](i);@xs[i];end;end;def run;c=C.new;c[0]<<2;c.xs;end",
        "[[1]]",
    );
}

#[test]
fn append_operators_return_the_method_value_without_rebinding_the_receiver() {
    run(
        "class C;property n;def initialize;@n=1;end;def <<(x:int);@n+=x;99;end;end;def run;c=C.new;v=c<<2;[v,c.n];end",
        "[99, 3]",
    );
    run(
        "class C;property n;def initialize;@n=1;end;def <<(x);@n+=x;self;end;end;def run;xs=[C.new];v=xs[0]<<2;[v==xs[0],xs[0].n];end",
        "[true, 3]",
    );
}

#[test]
fn operator_property_guards_and_ensure_preserve_partial_state() {
    for (definition, expression) in [
        ("def +(v);@xs.push(v);ensure;@n+=1;end", "c+false"),
        (
            "def []=(i,v);@xs[i]=v;ensure;@n+=1;end",
            "begin;c[0]=false;end",
        ),
        ("def <<(v);@xs.fill{v};ensure;@n+=1;end", "c<<false"),
    ] {
        let script = Engine::new().compile(&format!("class C;getter xs:array<int>;property n;def initialize;@xs=[1];@n=0;end;{definition};end;def run;c=C.new;begin;{expression};rescue;nil;end;[c.xs,c.n];end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "[[1], 1]", true);
    }
    run(
        "class C;property n;def initialize;@n=1;end;def +(x);@n+=2;raise 'stop';ensure;@n+=4;end;end;def run;c=C.new;begin;c+7;rescue;c.n;end;end",
        "7",
    );
}

#[test]
fn operator_receiver_unions_include_native_and_instance_paths() {
    for choose in [false, true] {
        let mut engine = Engine::new();
        engine.register("choose", move |_, _| Ok(Value::boolean(choose)));
        for (source, yes, no) in [
            (
                "class C;def +(x);7;end;end;def run;a=C.new;x=if choose();a;else;2;end;x+1;end",
                "7",
                "3",
            ),
            (
                "class C;def [](i);7;end;end;def run;a=C.new;x=if choose();a;else;[2];end;x[0];end",
                "7",
                "2",
            ),
            (
                "class C;def <<(x);7;end;end;def run;a=C.new;x=if choose();a;else;[2];end;x<<1;end",
                "7",
                "[2, 1]",
            ),
            (
                "class C;def []=(i,v);99;end;end;def run;a=C.new;x=if choose();a;else;[2];end;x[0]=7;end",
                "7",
                "7",
            ),
        ] {
            let script = engine.compile(source).unwrap();
            witness(
                &script,
                &[],
                &CallOptions::default(),
                if choose { yes } else { no },
                false,
            );
        }
    }
}

#[test]
fn operator_calls_preserve_ignored_host_block_transfers() {
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
    for (operator, expression) in [("+", "c+driver"), ("[]", "c[driver]"), ("<<", "c<<driver")] {
        for (transfer, expected) in [("break 7", "[7, 3]"), ("return 9", "[9, 3]")] {
            let script = Engine::new().compile(&format!("class C;property n;def initialize;@n=0;end;def {operator}(driver);driver.visit{{@n+=1;{transfer}}};ensure;@n+=2;end;end;def run(driver);c=C.new;v={expression};[v,c.n];end")).unwrap();
            witness(
                &script,
                std::slice::from_ref(&driver),
                &CallOptions::default(),
                expected,
                false,
            );
        }
    }
}

#[test]
fn operator_analysis_is_metered_interruptible_and_released() {
    for source in [
        "class C;property n;def initialize;@n=1;end;def +(x);@n+=x;self;end;end;def run;c=C.new;c+=2;c.n;end",
        "class C;property n;def initialize;@n=1;end;def [](i);@n;end;def []=(i,v);@n=v;99;end;end;def run;c=C.new;c[0]+=2;c.n;end",
        "class C;def ==(x);nil;end;end;def run;c=C.new;c != c;end",
    ] {
        metered(source);
    }
}

#[test]
fn operator_results_retain_exact_facts_at_known_call_sites() {
    for source in [
        "class C;def +(x);7;end;end;def run;C.new+2;end",
        "class C;def ==(x);nil;end;end;def run;c=C.new;c != c;end",
        "class C;def !=(x);7;end;def ==(x);true;end;end;def run;c=C.new;c != c;end",
        "class C;def [](i);7;end;end;def run;C.new[0];end",
        "class C;def []=(i,v);false;end;end;def run;c=C.new;c[0]=7;end",
        "class C;def <<(v);7;end;end;def run;C.new<<2;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut checked = check(&mut ctx, &script, &[], &CallOptions::default()).unwrap();
        assert!(
            checked.analysis.issues.data.is_empty(),
            "{source}: {checked:?}"
        );
        assert!(
            checked.analysis.incomplete.data.is_empty(),
            "{source}: {checked:?}"
        );
        let actual = script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value;
        let actual = observed(
            &mut ctx,
            &mut checked.facts,
            &script.inner.code.program,
            &actual,
        );
        assert_eq!(checked.analysis.returns, actual, "{source}: {checked:?}");
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
