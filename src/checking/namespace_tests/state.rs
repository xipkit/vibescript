use super::*;

#[test]
fn builtin_assignment_fallbacks_are_registered_before_initialization_and_type_lookup() {
    for (source, expected) in [
        ("module M;Math[:probe]=7;end;def run;0;end", "0"),
        (
            "module M;def self.write;Math.probe=7;end;end;def run;M.write;end",
            "7",
        ),
        (
            "module M;def self.write;Math[:probe]=[2];Math::probe[0]+=5;end;end;def run;M.write;end",
            "7",
        ),
        (
            "enum State;Ready;Done;end;module M;Math[:Status]=State;end;def run(x:Math.Status=:ready)->State;x;end",
            "State::Ready",
        ),
    ] {
        let script = Engine::legacy_unchecked().compile(source).unwrap();
        witness(&script, &[], &CallOptions::default(), expected, false);
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
    }
}

#[test]
fn initializer_fields_and_class_variables_follow_runtime_state() {
    for kind in ["module", "class"] {
        for (body, call, expected) in [
            ("C=7;def self.answer;C;end", "M.answer", "7"),
            ("def self.answer;@@count;end", "M.answer", "nil"),
            ("def self.answer;@@count=7;end", "M.answer", "7"),
            (
                "@@count=1;def self.add;@@count+=2;end;def self.answer;@@count;end",
                "M.add;M.answer",
                "3",
            ),
            ("C=[1];def self.add;C.push(2);end", "M.add;M::C", "[1]"),
            ("C=[1]", "M::C.push(2);M::C", "[1]"),
            ("C=[1]", "M.C.push(2);M::C", "[1]"),
            ("C=[1]", "M::C[0]=2;M::C", "[2]"),
            ("C=[1];def self.add;C[0]=2;end", "M.add;M::C", "[2]"),
            ("C={items:[1]}", "M::C.items.push(2);M::C", "{items: [1]}"),
            ("begin;C=3;ensure;C+=1;end", "M::C", "4"),
        ] {
            let source = format!("{kind} M;{body};end;def run;{call};end");
            let script = Engine::legacy_unchecked()
                .compile(&source)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            witness(&script, &[], &CallOptions::default(), expected, false);
        }
    }
}

#[test]
fn namespace_aliases_calls_blocks_and_rescues_share_fields() {
    for (body, expected, issues) in [
        ("a=M; a::C[0]=2;M::C", "[2]", false),
        ("M.apply { M::C[0]=3 };M::C", "[3]", false),
        ("M.apply { M::C[0]=4;break };M::C", "[4]", false),
        ("begin;M.fail;rescue;M::C;end", "[5]", false),
        ("begin;M.bad;rescue;M::C;end", "[6]", true),
        ("copy=M::C;copy.push(9);M::C", "[1]", false),
    ] {
        let script = Engine::legacy_unchecked().compile(&format!("module M;C=[1];def self.apply;yield;end;def self.fail;C[0]=5;raise(\"stop\");end;def self.bad;C[0]=6;1-\"bad\";end;end;def run;{body};end")).unwrap();
        witness(&script, &[], &CallOptions::default(), expected, issues);
    }
}

#[test]
fn namespace_pending_mutations_keep_the_original_index_across_calls() {
    for (body, expected) in [
        (
            "@@items=[1];def self.append;@@items.push(4);2;end;def self.result;@@items[-1]+=append();@@items;end",
            "[3, 4]",
        ),
        (
            "Items=[1];def self.append;Items.push(4);2;end;def self.result;Items[-1]+=append();Items;end",
            "[3]",
        ),
    ] {
        run(
            &format!("module M;{body};end;def run;M.result;end"),
            expected,
        );
    }
    run(
        "module M;Items=[1];def self.append;Items.push(4);2;end;end;def run;M::Items[-1]+=M.append;M::Items;end",
        "[3]",
    );
}

#[test]
fn namespace_initialization_precedes_defaults_and_does_not_repeat_nested_bodies() {
    run(
        "module M;C=M;def self.answer;7;end;end;def run;M::C::C.answer;end",
        "7",
    );
    run(
        "module M;C=N::D;module N;D=[2];end;end;def run(x=M::C);[x,M::N::D];end",
        "[[2], [2]]",
    );
    run(
        "module A;C=B.value;end;module B;@@v=7;def self.value;@@v;end;end;def run;[A::C,B.value];end",
        "[nil, 7]",
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::legacy_unchecked();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let script = engine
        .compile("module M;C=tick();end;def run(x=M::C);x;end")
        .unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn missing_namespace_variables_and_partial_initializers_keep_error_state() {
    for (source, expected) in [
        (
            "module M;def self.get;@x;end;end;def run;begin;M.get;rescue RuntimeError;99;end;end",
            "99",
        ),
        (
            "module M;def self.get;@x=1;end;end;def run;begin;M.get;rescue RuntimeError;99;end;end",
            "99",
        ),
        (
            "module M;def self.fill;@@missing.push(1);end;end;def run;begin;M.fill;rescue RuntimeError;M::missing;end;end",
            "nil",
        ),
        (
            "module M;begin;C=1;missing();rescue RuntimeError;C+=1;ensure;C+=2;end;end;def run;M::C;end",
            "4",
        ),
        (
            "module M;end;def run;begin;M::missing;rescue RuntimeError;99;end;end",
            "99",
        ),
    ] {
        let script = Engine::legacy_unchecked().compile(source).unwrap();
        witness(&script, &[], &CallOptions::default(), expected, true);
    }
}

#[test]
fn namespace_setters_keep_assignment_results_and_evaluation_order() {
    for (source, expected) in [
        ("module M;end;def run;M.value=3;M.value;end", "3"),
        ("module M;C=3;end;def run;M.C+=4;M::C;end", "7"),
        (
            "module M;def self.value=(x:int);@@value=x+1;99;end;end;def assign;M.value=5;end;def run;[assign,M.value];end",
            "[5, 6]",
        ),
        (
            "module M;@@n=1;def self.value;@@n;end;def self.value=(x);@@n=x*2;99;end;end;def run;M.value+=3;M.value;end",
            "8",
        ),
        (
            "module A;C=1;end;module B;C=2;end;def run;m=A;m.C=(begin;m=B;7;end);[A::C,B::C];end",
            "[1, 7]",
        ),
        (
            "module A;C=1;end;module B;C=2;end;def run;m=A;m.C+=(begin;m=B;7;end);[A::C,B::C];end",
            "[8, 2]",
        ),
        (
            "module M;C=[1];end;def run;M::C[0]+=(begin;M.C=[9];7;end);M.C;end",
            "[9]",
        ),
        (
            "module M;protected def self.value=(x);@@value=x;end;def self.set;self.value=7;end;end;def run;M.set;M.value;end",
            "7",
        ),
        (
            "module M;C={items:[1]};end;def run;M::C.items[0]=2;M::C;end",
            "{items: [2]}",
        ),
    ] {
        run(source, expected);
    }
    for setter in [
        "private def self.value=(x);@@v=x;end",
        "def self.value=(x:int);@@v=x;end",
    ] {
        let script = Engine::legacy_unchecked().compile(&format!("module M;{setter};end;def run;a=[];begin;M.value=a.push(1);rescue RuntimeError;nil;end;a;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "[1]", true);
    }
}

#[test]
fn conditional_constants_preserve_missing_fields_and_root_precedence() {
    for flag in [true, false] {
        for (body, expected, issues) in [
            ("C", if flag { "7" } else { "9" }, false),
            (
                "begin;C();rescue RuntimeError;11;end",
                if flag { "11" } else { "9" },
                true,
            ),
            (
                "begin;(C rescue missing)();rescue RuntimeError;11;end",
                if flag { "11" } else { "9" },
                true,
            ),
        ] {
            let mut engine = Engine::legacy_unchecked();
            engine.register("flag", move |_, _| Ok(Value::boolean(flag)));
            let mut options = CallOptions::default();
            if body == "C" {
                options.globals.insert("C".into(), Value::int(9));
            } else {
                engine.register("C", |_, _| Ok(Value::int(9)));
            }
            let script = engine
                .compile(&format!(
                    "module M;if flag();C=7;end;def self.answer;{body};end;end;def run;M.answer;end"
                ))
                .unwrap();
            witness(&script, &[], &options, expected, issues);
        }
    }
    let mut deep = Value::nil();
    for _ in 0..129 {
        deep = Value::array(vec![deep]);
    }
    for call in ["C()", "(C rescue missing)()"] {
        let script = Engine::legacy_unchecked()
            .compile(&format!(
                "module M;C=7;def self.answer;{call};end;end;def run;M.answer;end"
            ))
            .unwrap();
        let options = CallOptions {
            globals: [("C".into(), deep.clone())].into(),
            ..CallOptions::default()
        };
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(!report.diagnostics.is_empty(), "{report:?}");
        assert_ne!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Recursion
        );
    }
}

#[test]
fn initializer_failures_precede_entry_shapes_defaults_and_handlers() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::legacy_unchecked();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let source = "module M\n C=missing()\nend\ndef run(required, default=effect())\n begin;effect();rescue;99;end\nend";
    let script = engine.compile(source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert_eq!(report.diagnostics[0].position.line, 2);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let script = engine
        .compile("module M;C=3;end;def run(required, default=effect());effect();end")
        .unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Argument
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn namespace_state_survives_ignored_host_control_transfers_without_replaying_blocks() {
    let method = HostMethod::new_with_block("visit", |host, _, _| {
        for _ in 0..3 {
            let _ = host.call_block(&[]);
        }
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
        let script = Engine::legacy_unchecked().compile(&format!("module M;@@n=0;def self.go(driver);driver.visit{{@@n+=1;{transfer}}};end;end;def run(driver);[M.go(driver),M.n];end")).unwrap();
        witness(
            &script,
            std::slice::from_ref(&driver),
            &CallOptions::default(),
            expected,
            false,
        );
    }
    let script = Engine::legacy_unchecked()
        .compile("module M;@@n=0;driver.visit{@@n+=1;return 7};@@n=99;end;def run;M.n;end")
        .unwrap();
    let options = CallOptions {
        globals: [("driver".into(), driver)].into(),
        ..CallOptions::default()
    };
    witness(&script, &[], &options, "99", false);
    run(
        "module M;@@n=0;begin;[1].each {begin;@@n=1;return 7;rescue RuntimeError;@@n=99;ensure;@@n+=2;end};rescue LocalJumpError;@@n+=4;end;end;def run;M.n;end",
        "7",
    );
}

#[test]
fn namespace_initializers_and_state_are_metered_interruptible_and_released() {
    for source in [
        "module M;@@items=[1];def self.add;@@items.push(4);2;end;def self.result;@@items[-1]+=add();@@items;end;end;def run;M.result;end",
        "module M;C=3;def self.value=(x);C=x+1;99;end;end;def run;M.value=4;M::C;end",
        "module M;C=3;module N;D=[2];end;end;def run;M::N::D[0]=M::C;M::N::D;end",
        "module M;def self.work;Math[:items]=[1,2];Math[:items][-1]+=5;end;end;def run;M.work;end",
    ] {
        metered(source);
    }
}
