mod common;

use vibescript::{CallOptions, Engine, Script, Value};

fn values(source: &str) -> (Value, Value) {
    let script = common::gradual_engine().compile(source).unwrap();
    let class = script
        .call("klass", &[], CallOptions::default())
        .unwrap()
        .value;
    let instance = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    (class, instance)
}

fn check(script: &Script, options: &CallOptions, clean: bool) {
    let report = script.check(options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.is_clean(), clean, "{report:?}");
}

fn general(script: &Script, options: &CallOptions, clean: bool) {
    let report = script.check_function("run", options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.is_clean(), clean, "{report:?}");
}

#[test]
fn whole_file_inputs_use_the_defining_property_and_constructor() {
    let (class, instance) = values(
        "class Box;property n:int;def initialize(n:int=5);@n=n;end;def value->int;@n;end;end;def klass;Box;end;def make;Box.new;end",
    );
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    for (source, clean) in [
        ("def run(x:Foreign)->int;x.n;end", true),
        ("def run(x:Foreign)->int;x.value;end", true),
        ("def run(x:Foreign)->string;x.n;end", false),
        ("def run(x:Foreign);x.n='bad';end", false),
    ] {
        let script = common::gradual_engine().compile(source).unwrap();
        assert_eq!(
            script
                .call("run", std::slice::from_ref(&instance), options.clone())
                .is_ok(),
            clean,
            "{source}"
        );
        check(&script, &options, clean);
    }
}

#[test]
fn single_function_inputs_preserve_conservative_fields_and_typed_writes() {
    let (class, instance) = values(
        "class Box;property n:int;def initialize;@n=5;end;def answer;7;end;end;def klass;Box;end;def make;Box.new;end",
    );
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    for (source, clean) in [
        ("def run(x:Foreign)->int;x.n=7;x.n;end", true),
        ("def run(x:Foreign)->int;x.answer;end", true),
        ("def run(x:Foreign)->int;x.n;end", false),
        ("def run(x:Foreign);x.n='bad';end", false),
        ("def run(x:Foreign)->string;x.answer;end", false),
    ] {
        let script = common::gradual_engine().compile(source).unwrap();
        general(&script, &options, clean);
        if clean {
            assert_eq!(
                script
                    .call("run", std::slice::from_ref(&instance), options.clone())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
        }
    }
}

#[test]
fn foreign_inputs_preserve_possible_aliases_and_distinguish_new_objects() {
    let (class, instance) =
        values("class Box;property n;end;def klass;Box;end;def make;Box.new;end");
    let options = CallOptions {
        globals: [
            ("Foreign".into(), class),
            ("other".into(), instance.clone()),
        ]
        .into(),
        ..Default::default()
    };
    for (body, clean) in [
        ("a.n=7;b.n=false;a.n", false),
        ("a.n=7;other.n=false;a.n", false),
        ("other.n=7;a.n=false;other.n", false),
        ("if a==b;false;else;7;end", false),
        ("if a==other;false;else;7;end", false),
        ("if a.equal?(b);false;else;7;end", false),
        ("a.n=7;c=Foreign.new;c.n=false;a.n", true),
        ("c=Foreign.new;c.n=7;a.n=false;c.n", true),
        ("c=Foreign.new;if a==c;false;else;7;end", true),
        ("a.n=false;a.n=7;a.n", true),
    ] {
        let script = common::gradual_engine()
            .compile(&format!("def run(a:Foreign,b:Foreign)->int;{body};end"))
            .unwrap();
        let result = script.call(
            "run",
            &[instance.clone(), instance.clone()],
            options.clone(),
        );
        assert_eq!(result.is_ok(), clean, "{body}: {result:?}");
        general(&script, &options, clean);
        check(&script, &options, clean);
    }
}

#[test]
fn foreign_classes_in_collection_and_nullable_inputs_keep_their_methods_and_aliases() {
    let (class, instance) =
        values("class Box;property n;def answer;7;end;end;def klass;Box;end;def make;Box.new;end");
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    for (source, argument, clean) in [
        (
            "def run(a:Foreign?)->int;if a.nil?;7;else;a.answer;end;end",
            Value::nil(),
            true,
        ),
        (
            "def run(a:array<Foreign>)->array<int>;a.map{|x|x.answer};end",
            Value::array(vec![instance.clone()]),
            true,
        ),
        (
            "def run(a:array<Foreign?>)->array<int>;a.map{|x|if x.nil?;7;else;x.answer;end};end",
            Value::array(vec![Value::nil(), instance.clone()]),
            true,
        ),
        (
            "def run(a:array<Foreign>)->array<int>;a.map{|x|x.n=7;a.each{|y|y.n=false};x.n};end",
            Value::array(vec![instance.clone()]),
            false,
        ),
        (
            "def run(a:array<Foreign>)->array<string>;a.map{|x|x.answer};end",
            Value::array(vec![instance.clone()]),
            false,
        ),
        (
            "def run(a:hash<string,Foreign>)->int;x=a[:left];if x.nil?;7;else;x.answer;end;end",
            Value::hash(vec![(b"left".to_vec(), instance.clone())]),
            true,
        ),
        (
            "def run(a:{left:Foreign,right:Foreign})->int;a[:left].n=7;a[:right].n=false;a[:left].n;end",
            Value::hash(vec![
                (b"left".to_vec(), instance.clone()),
                (b"right".to_vec(), instance.clone()),
            ]),
            false,
        ),
    ] {
        let script = common::gradual_engine().compile(source).unwrap();
        assert_eq!(
            script.call("run", &[argument], options.clone()).is_ok(),
            clean,
            "{source}"
        );
        general(&script, &options, clean);
        check(&script, &options, clean);
    }
}

#[test]
fn equal_class_names_from_different_sources_keep_separate_domains_and_heaps() {
    let (a, first) = values(
        "class Box;property n:int;def initialize;@n=5;end;end;def klass;Box;end;def make;Box.new;end",
    );
    let (b, second) = values(
        "class Box;property n:string;def initialize;@n='yes';end;end;def klass;Box;end;def make;Box.new;end",
    );
    let options = CallOptions {
        globals: [("A".into(), a), ("B".into(), b)].into(),
        ..Default::default()
    };
    for (body, clean) in [
        ("a.n=7;b.n='ok';a.n+b.n.length", true),
        ("if a==b;false;else;7;end", true),
        ("a.n='wrong';7", false),
        ("b.n=7;7", false),
    ] {
        let script = common::gradual_engine()
            .compile(&format!(
                "class Box;property n:bool;def initialize;@n=false;end;end;def run(a:A,b:B)->int;{body};end"
            ))
            .unwrap();
        assert_eq!(
            script
                .call("run", &[first.clone(), second.clone()], options.clone())
                .is_ok(),
            clean,
            "{body}"
        );
        general(&script, &options, clean);
        check(&script, &options, clean);
    }
    let script = common::gradual_engine()
        .compile("def run(a:A)->B;a;end")
        .unwrap();
    general(&script, &options, false);
    check(&script, &options, false);
}

#[test]
fn foreign_constructor_summaries_preserve_missing_fields_and_recursive_inputs() {
    for (constructor, make, clean) in [
        (
            "def initialize(flag:bool=false);if flag;@n=7;else;@n=8;end;end",
            "Box.new",
            true,
        ),
        (
            "def initialize(flag:bool=false);if flag;@n=7;end;end",
            "Box.new",
            false,
        ),
        (
            "def initialize;begin;return;ensure;@n=7;end;end",
            "Box.new",
            true,
        ),
        (
            "def initialize;assign(7);end;def assign(n:int);@n=n;end",
            "Box.new",
            true,
        ),
        (
            "def initialize(other:Box?=nil);if other;@n=other.n;else;@n=7;end;end",
            "Box.new(Box.new)",
            true,
        ),
    ] {
        let (class, instance) = values(&format!(
            "class Box;property n:int;{constructor};end;def klass;Box;end;def make;{make};end"
        ));
        let options = CallOptions {
            globals: [("Foreign".into(), class)].into(),
            ..Default::default()
        };
        let script = common::gradual_engine()
            .compile("def run(x:Foreign)->int;x.n;end")
            .unwrap();
        assert_eq!(
            script.call("run", &[instance], options.clone()).is_ok(),
            clean,
            "{constructor}"
        );
        check(&script, &options, clean);
    }
}

#[test]
fn checking_foreign_inputs_never_executes_initializers_constructors_or_callbacks() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vibescript::{HostMethod, Signature};

    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = common::gradual_engine();
    engine.register_method(
        "probe",
        HostMethod::new("probe", move |_, _, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(Value::int(7))
        })
        .with_signature(Signature {
            params: vec![],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    let source = engine.compile("class Box;K=probe();property n:int;def initialize;@n=probe();end;def answer->int;probe();end;end;def klass;Box;end;def make;Box.new;end").unwrap();
    let class = source
        .call("klass", &[], CallOptions::default())
        .unwrap()
        .value;
    let instance = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    let receiver = common::gradual_engine()
        .compile("def run(x:Foreign)->int;x.n=7;x.answer;end")
        .unwrap();
    let before = effects.load(Ordering::Relaxed);
    general(&receiver, &options, true);
    check(&receiver, &options, true);
    assert_eq!(effects.load(Ordering::Relaxed), before);
    assert_eq!(
        receiver
            .call("run", &[instance], options)
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert!(effects.load(Ordering::Relaxed) > before);
}

#[test]
fn foreign_types_survive_defaults_and_variadic_parameter_binding() {
    let (class, instance) =
        values("class Box;def answer;7;end;end;def klass;Box;end;def make;Box.new;end");
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    for (source, args, keywords) in [
        (
            "def run(a:Foreign=Foreign.new)->int;a.answer;end",
            vec![],
            vec![],
        ),
        (
            "def run(*a:array<Foreign>)->array<int>;a.map{|x|x.answer};end",
            vec![instance.clone(), instance.clone()],
            vec![],
        ),
        (
            "def run(a:Foreign:)->int;a.answer;end",
            vec![],
            vec![("a".to_owned(), instance.clone())],
        ),
        (
            "def run(**a:hash<string,Foreign>)->array<int>;a.values.map{|x|x.answer};end",
            vec![],
            vec![("a".to_owned(), instance.clone())],
        ),
    ] {
        let script = common::gradual_engine().compile(source).unwrap();
        script
            .call_with_keywords("run", &args, &keywords, options.clone())
            .unwrap();
        general(&script, &options, true);
        check(&script, &options, true);
    }
}

#[test]
fn pending_foreign_field_targets_keep_reentrant_alias_writes() {
    let (class, instance) =
        values("class Box;property items;end;def klass;Box;end;def make;Box.new;end");
    let options = CallOptions {
        globals: [("Foreign".into(), class)].into(),
        ..Default::default()
    };
    let script = common::gradual_engine().compile("def replace(c:Foreign);c.items=['bad'];7;end;def run(a:Foreign,b:Foreign)->array<int>;a.items=[1];a.items[0]=replace(b);a.items;end").unwrap();
    assert!(
        script
            .call("run", &[instance.clone(), instance], options.clone())
            .is_err()
    );
    general(&script, &options, false);
    check(&script, &options, false);
}
