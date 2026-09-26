use super::*;

#[test]
fn templates_render_scalars_symbols_and_nested_words() {
    run(
        r##"def run;"a#{1}:#{false}:#{nil}:#{:raw}";end"##,
        "a1:false::raw",
    );
    run(r##"def run;%W[a#{1} b#{2}];end"##, "[a1, b2]");
    run(r##"def run;%I[a#{1} b#{2}];end"##, "[a1, b2]");
    run(
        r##"class C;end;def run;nil.is_type?("C#{''}?");end"##,
        "true",
    );
    run(r##"def run;"a#{"b#{2}"}c";end"##, "ab2c");
    run(r##"def run;"#{"\xff\x00"}".bytes;end"##, "[255, 0]");
}

#[test]
fn rendering_calls_eligible_methods_once_and_ignores_visibility() {
    for access in ["public", "protected", "private"] {
        run(
            &format!(
                r##"class C;property n;def initialize;@n=0;end;{access};def to_s(*rest,prefix:'x');@n+=1;prefix;end;end;def run;c=C.new;["#{{c}}#{{c}}",c.n];end"##
            ),
            "[xx, 2]",
        );
    }
    for method in [
        "def to_s;7;end",
        "def to_s;:raw;end",
        "def to_s(x);raise 'called';end",
        "def to_s(x:);raise 'called';end",
    ] {
        run(
            &format!(r##"class C;{method};end;def run;"#{{C.new}}";end"##),
            "<C instance>",
        );
    }
    run(
        r##"module M;def self.to_s;raise 'called';end;end;def run;"#{M}";end"##,
        "<Class M>",
    );
    let method = HostMethod::new("to_s", |_, _, _| panic!("converted a host object"));
    let object = Value::object(vec![(b"to_s".to_vec(), method.value())]);
    let script = Engine::legacy_unchecked()
        .compile(r##"def run(object);"#{object}";end"##)
        .unwrap();
    witness(
        &script,
        &[object],
        &CallOptions::default(),
        "<object>",
        false,
    );
}

#[test]
fn rendering_preserves_object_state_and_operand_order() {
    run(
        r##"class C;property n;def initialize;@n=0;end;def to_s;@n+=1;"#{@n}";end;end;def run;c=C.new;text="#{c}#{begin;c.n+=10;c;end}#{c.n}";[text,c.n];end"##,
        "[11212, 12]",
    );
    run(
        r##"class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;text="#{[c]}#{ {c:c} }";[text,c.n];end"##,
        "[[<C instance>]{c: <C instance>}, 0]",
    );
    run(
        r##"class C;property n;def initialize;@n=0;end;def to_s;@n+=1;"#{self.class}:#{@n}";end;end;def run;c=C.new;["#{c}",c.n];end"##,
        "[<Class C>:1, 1]",
    );
    run(
        r##"class C;def to_s;'original';end;end;class D;end;def run;c=C.new;C=D;"#{c}/#{C}";end"##,
        "original/<Class D>",
    );
}

#[test]
fn rendering_errors_keep_partial_effects_and_discard_abandoned_templates() {
    run(
        r##"class C;property n;def initialize;@n=0;end;def to_s;@n+=1;raise 'bad';ensure;@n+=2;end;end;def run;c=C.new;n=0;begin;"before#{c}#{begin;n+=1;end}after";rescue;nil;end;["done#{7}",n,c.n];end"##,
        "[done7, 0, 3]",
    );
    run(
        r##"def run;n=0;"start#{begin;"bad#{begin;raise 'bad';end}";rescue;"ok#{begin;n+=1;end}";end}end";end"##,
        "startok1end",
    );
    run(
        r##"def run;n=0;begin;"#{begin;n+=1;raise 'again' if n<2;n;end}";rescue;retry;end;end"##,
        "2",
    );
}

#[test]
fn template_cleanup_preserves_loop_and_nonlocal_block_transfers() {
    run(
        r##"def run;n=0;for i in 1..3;"#{begin;next if i<3;n+=1;end}";end;"n=#{n}";end"##,
        "n=1",
    );
    run(
        r##"def run;"a#{begin;for i in 1..3;"#{begin;break 7;end}";end;end}b";end"##,
        "a7b",
    );
    run(
        r##"def once;yield;end;def run;once{"discard#{begin;return "kept#{7}";end}"};end"##,
        "kept7",
    );
}

#[test]
fn rendering_keeps_property_guards_and_method_return_contracts() {
    let script=Engine::legacy_unchecked().compile(r##"class C;property n:int;def initialize;@n=1;end;def to_s;@n=false;'bad';end;end;def run;c=C.new;begin;"#{c}";rescue;nil;end;c.n;end"##).unwrap();
    witness(&script, &[], &CallOptions::default(), "1", true);
    let script = Engine::legacy_unchecked()
        .compile(
            r##"class C;def to_s -> int;'bad';end;end;def run;begin;"#{C.new}";rescue;99;end;end"##,
        )
        .unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
}

#[test]
fn rendering_preserves_ignored_host_block_returns_and_cleanup() {
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
    let script=Engine::legacy_unchecked().compile(r##"class C;property n;def initialize(driver);@driver=driver;@n=0;end;def to_s;@driver.visit{@n+=1;return 'ok'};'bad';ensure;@n+=2;end;end;def run(driver);c=C.new(driver);["#{c}",c.n];end"##).unwrap();
    witness(
        &script,
        &[driver],
        &CallOptions::default(),
        "[ok, 3]",
        false,
    );
}

#[test]
fn rendering_is_metered_interruptible_and_releases_all_pending_text() {
    let mut engine = Engine::legacy_unchecked();
    engine.set_output_writer(|_, _| panic!("checker invoked output writer"));
    for source in [
        r##"def run;"a#{"b#{7}"}c";end"##,
        r##"class C;property n;def initialize;@n=0;end;def to_s;@n+=1;"x#{@n}";end;end;def run;c=C.new;"#{c}#{c}";end"##,
        r##"def run;begin;"bad#{begin;raise 'bad';end}";rescue;"ok#{7}";end;end"##,
        "class C;def to_s;'ok';end;end;def run;puts(C.new);end",
        "class C;def to_s;'ok';end;end;def run;p(C.new);end",
    ] {
        metered_script(&engine.compile(source).unwrap());
    }
}

#[test]
fn format_converts_direct_instances_in_order_before_pattern_errors() {
    run(
        "class C;def to_s;raise 'called';end;end;def run;c=C.new;['%s'%c,'%s'%C,'%s'%[c]];end",
        "[<C instance>, <Class C>, <C instance>]",
    );
    run(
        "class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;[format('%s/%s',c,[c]),c.n];end",
        "[x/[<C instance>], 1]",
    );
    run(
        "class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;begin;sprintf('%d',c,c);rescue;nil;end;c.n;end",
        "2",
    );
    for helper in ["format", "sprintf"] {
        let script=Engine::legacy_unchecked().compile(&format!("class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;begin;{helper}(7,c);rescue;nil;end;c.n;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "0", true);
    }
    metered(
        "class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;[format('%s%s',c,c),c.n];end",
    );
}

#[test]
fn output_converts_each_direct_operand_and_inspection_preserves_values() {
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let mut engine = Engine::legacy_unchecked();
    engine.set_output_writer(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let count = writes.clone();
    engine.set_error_writer(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let script=engine.compile("class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;puts(c,c);print([c]);warn(c);x=p(c);a=[1];b=p(a);b.push(2);[x==c,c.n,a,b];end").unwrap();
    witness(
        &script,
        &[],
        &CallOptions::default(),
        "[true, 3, [1], [1, 2]]",
        false,
    );
    assert_eq!(writes.load(Ordering::Relaxed), 6);
    let script = engine
        .compile("def run;[puts(),print(),warn(),p()];end")
        .unwrap();
    witness(
        &script,
        &[],
        &CallOptions::default(),
        "[nil, nil, nil, nil]",
        false,
    );
    assert_eq!(writes.load(Ordering::Relaxed), 7);
}

#[test]
fn output_errors_preserve_conversion_effects_and_skip_later_operands() {
    run(
        "class C;def to_s -> int;false;end;end;def run;begin;puts(C.new);rescue;7;end;end",
        "7",
    );
    for missing in ["puts", "warn"] {
        let mut engine = Engine::legacy_unchecked();
        if missing == "puts" {
            engine.set_error_writer(|_, _| panic!("used the wrong writer"));
        } else {
            engine.set_output_writer(|_, _| panic!("used the wrong writer"));
        }
        let script = engine
            .compile(&format!("class C;def to_s -> int;false;end;end;def run;begin;{missing}(C.new);rescue;7;end;end"))
            .unwrap();
        witness(&script, &[], &CallOptions::default(), "7", false);
    }
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let mut engine = Engine::legacy_unchecked();
    engine.set_output_writer(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Err(crate::Error::new(ErrorKind::Runtime, "writer failed"))
    });
    let script=engine.compile("class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;begin;puts(c,c);rescue;nil;end;c.n;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "1", false);
    assert_eq!(writes.load(Ordering::Relaxed), 1);
    run(
        "class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;begin;puts(c,c);rescue;nil;end;c.n;end",
        "0",
    );
}

#[test]
fn rendering_rejects_keywords_and_blocks_before_conversions_or_writes() {
    let mut engine = Engine::legacy_unchecked();
    engine.set_output_writer(|_, _| panic!("invalid call reached writer"));
    engine.set_error_writer(|_, _| panic!("invalid call reached writer"));
    for call in [
        "puts(c,a:7)",
        "warn(c){c.n=99}",
        "print(c){c.n=99}",
        "p(c,a:7)",
        "format('%s',c,a:7)",
        "sprintf('%s',c){c.n=99}",
    ] {
        let script=engine.compile(&format!("class C;property n;def initialize;@n=0;end;def to_s;@n+=1;'x';end;end;def run;c=C.new;begin;{call};rescue;nil;end;c.n;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "0", true);
    }
}
