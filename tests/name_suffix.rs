use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Limits, Value, diagnostic::Code,
    stringify_json,
};

fn run(source: &str) -> String {
    let value = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap()
        .value;
    let json = stringify_json(&value, CallOptions::default()).unwrap();
    String::from_utf8(json.value.as_bytes().unwrap().to_vec()).unwrap()
}

#[test]
fn adjacent_inequality_compares_without_assigning() {
    for source in [
        "a = 1; b = 2; [a!=b, a==1]",
        "x = 2; [x!=3, x==2]",
        "A = 1; B = 2; [A!=B, A==1]",
        "def a -> int; 1; end; def b -> int; 2; end; [a!=b, a==1]",
        "class Pair; @l: int = 1; @r: int = 2; def compare -> array<bool>; [@l!=@r, @l==1]; end; end; Pair.new.compare",
        "class Pair; @@l: int = 1; @@r: int = 2; def self.compare -> array<bool>; [@@l!=@@r, @@l==1]; end; end; Pair.compare",
        "class Pair; def left -> int; 1; end; def right -> int; 2; end; end; p = Pair.new; [p.left!=p.right, p.left==1]",
        "module M; A = 1; B = 2; end; [M::A!=M::B, M::A==1]",
    ] {
        assert_eq!(run(source), "[true,true]", "{source}");
    }
}

#[test]
fn method_suffixes_survive_call_forms_and_operators() {
    for source in [
        "def ok? -> bool; true; end; ok?==true",
        "def ok? -> bool; true; end; ok?!=false",
        "def OK? -> bool; true; end; OK?==true",
        "def save! -> bool; true; end; save!==true",
        "def save! -> bool; true; end; save!!=false",
        "def text? -> string; 'yes'; end; (text?=~/yes/) == 0",
        "def text! -> string; 'yes'; end; (text!=~/yes/) == 0",
        "def ok?(n: int) -> bool; n > 0; end; ok? 1",
        "def save!(n: int) -> bool; n > 0; end; save!(1)",
        "class User; def valid? -> bool; true; end; def save! -> bool; true; end; end; user = User.new; user.valid? && user.save!",
        "list: array<int>? = []; list&.empty? == true",
        "list: array<int>? = nil; list&.empty? == nil",
        "def ok? -> bool; true; end; alias ready? ok?; ready?",
        "[:ok?.to_s, :save!.to_s] == ['ok?', 'save!']",
        "def ok? -> bool; true; end; \"#{ok?==true}\" == 'true'",
    ] {
        assert_eq!(run(source), "true", "{source}");
    }
}

#[test]
fn labels_are_strings_and_optional_markers_are_not_name_suffixes() {
    for source in [
        "h = { ready?: true, save!: false }; h['ready?'] && !h['save!']",
        "def ready? -> bool; true; end; { ready?: }['ready?']",
        "def ready? -> bool; true; end; def f(**kw: hash<string, bool>) -> bool; kw.fetch('ready?'); end; f(ready?:)",
        "def f(**opts: hash<string, bool>) -> bool; opts.fetch('ready?'); end; f(ready?: true)",
        "def f(**opts: hash<string, bool>) -> bool; opts.fetch('ready?'); end; f ready?: true",
        "def f(**opts: hash<string, bool>) -> bool; opts.fetch('save!'); end; f save!:true",
        "def f(*, ready: bool) -> bool; ready; end; f(ready: true)",
        "def f(&block?: () -> bool) -> bool; if block_given?; yield; else; true; end; end; f",
        "h: { ready?: bool } = {}; h['ready'] == nil",
        "x: string?=nil; x == nil",
    ] {
        assert_eq!(run(source), "true", "{source}");
    }
}

#[test]
fn optional_type_markers_before_defaults_keep_their_meaning() {
    for (source, expected) in [
        ("def f(x: int?=nil) -> int?; x; end; f", "null"),
        ("def f(x: int?= nil) -> int?; x; end; f", "null"),
        ("def f(x:int?=nil) -> int?; x; end; f(2)", "2"),
        ("def f(x: int?=\nnil) -> int?; x; end; f", "null"),
        ("def f(x: bool?=true) -> bool?; x; end; f", "true"),
        ("def f(x: string?=\"a\") -> string?; x; end; f", "\"a\""),
        ("class K; end; def f(x: K?=nil) -> K?; x; end; f", "null"),
        (
            "enum E; A; end; def f(x: E?=nil) -> E?; x; end; f == nil",
            "true",
        ),
        (
            "enum E; A; end; def f(x: E?=E::A) -> E?; x; end; f == E::A",
            "true",
        ),
        (
            "class C; @x: int?; def initialize(x: int?=nil); @x = x; end; def x -> int?; @x; end; end; C.new.x",
            "null",
        ),
        (
            "class C; @x: int?; def initialize(x: int?=3); @x = x; end; def x -> int?; @x; end; end; C.new.x",
            "3",
        ),
        ("def f x: int?=nil -> int?\n  x\nend\nf", "null"),
        ("def f x: int?=4, y: bool?=nil -> int?\n  x\nend\nf", "4"),
        (
            "def f(x: int?=nil, y: bool?=false) -> array<any>; [x, y]; end; f",
            "[null,false]",
        ),
        ("def f(*, x: int?=nil) -> int?; x; end; f", "null"),
        ("x: int?=nil; x", "null"),
        ("X: string?=nil; X", "null"),
        (
            "class C; @x: int?=nil; def x -> int?; @x; end; end; C.new.x",
            "null",
        ),
    ] {
        assert_eq!(run(source), expected, "{source}");
        let checked = Engine::new().type_check(source).unwrap();
        assert!(
            checked.diagnostics.is_empty(),
            "{source}: {:?}",
            checked.diagnostics
        );
    }
}

#[test]
fn bindings_reject_suffixes_with_applicable_fixes() {
    for (source, fixed) in [
        ("READY? = 1", "READY = 1"),
        ("x! = 3", "x = 3"),
        ("if? = 3", "if_ = 3"),
        ("x?=3", "x=3"),
        ("é! = 3", "é = 3"),
        ("x?: bool = true", "x: bool = true"),
        ("x! += 3", "x += 3"),
        ("x!, y = [1, 2]", "x, y = [1, 2]"),
        ("for x! in [1]; end", "for x in [1]; end"),
        ("[1].each { |x!| x }", "[1].each { |x| x }"),
        ("def f(ok?: bool); end", "def f(ok: bool); end"),
        (
            "def f(*ok!: array<int>); end",
            "def f(*ok: array<int>); end",
        ),
        (
            "def f(**ok?: hash<string, bool>); end",
            "def f(**ok: hash<string, bool>); end",
        ),
        ("def f(*, ok?: bool); end", "def f(*, ok: bool); end"),
        ("def f(&block!: ()); end", "def f(&block: ()); end"),
        ("@done? = true", "@done = true"),
        ("@@done! = true", "@@done = true"),
        ("@done?", "@done"),
        ("@@done!", "@@done"),
        ("M::READY?", "M::READY"),
        (
            "class A; @done?: bool = true; end",
            "class A; @done: bool = true; end",
        ),
        (
            "class A; @@done!: bool = true; end",
            "class A; @@done: bool = true; end",
        ),
        (
            "class A; property done?: bool; end",
            "class A; property done: bool; end",
        ),
        (
            "class A; def initialize(@done?: bool); end; end",
            "class A; def initialize(@done: bool); end; end",
        ),
        ("class Ready?; end", "class Ready; end"),
        ("module Ready!; end", "module Ready; end"),
        ("enum Ready; Done?; end", "enum Ready; Done; end"),
        ("type Ready! = bool", "type Ready = bool"),
        (
            "begin; 1; rescue => error!; 2; end",
            "begin; 1; rescue => error; 2; end",
        ),
    ] {
        let error = Engine::new().compile(source).err().expect(source);
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        let fix = diagnostic.applicable_fix().expect(source);
        assert_eq!(fix.apply(source).unwrap(), fixed, "{source}");
        if let Err(error) = Engine::new().compile(fixed) {
            assert_ne!(error.kind, ErrorKind::Syntax, "{fixed}: {error}");
        }
    }
}

#[test]
fn member_assignment_targets_reject_method_suffixes() {
    for suffix in ['?', '!'] {
        for op in [
            "=", "+=", "-=", "*=", "/=", "//=", "%=", "**=", "||=", "&&=",
        ] {
            for target in [
                format!("h.ready{suffix}"),
                format!("((h.ready{suffix}))"),
                format!("h&.ready{suffix}"),
                format!("h&.inner.ready{suffix}"),
                format!("h.inner&.ready{suffix}"),
                format!("h.\n  réady{suffix}"),
            ] {
                let source = format!("def f(h: any); {target} {op} 1; end");
                let at = source.rfind(suffix).unwrap();
                let error = Engine::new().compile(&source).err().expect(&source);
                assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
                let diagnostic = &error.diagnostics()[0];
                assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
                assert_eq!(
                    diagnostic.span,
                    vibescript::diagnostic::Span::new(at, at + 1)
                );
                let mut expected = source.clone();
                expected.remove(at);
                assert_eq!(
                    diagnostic.applicable_fix().unwrap().apply(&source).unwrap(),
                    expected
                );
            }
        }
        for targets in [
            format!("h.ready{suffix}, other"),
            format!("other, h.ready{suffix}"),
            format!("other, (h.ready{suffix}, last)"),
            format!("other, [h.ready{suffix}, last]"),
            format!("other, *h.ready{suffix}"),
            format!("other, h&.ready{suffix}"),
            format!("h&.ready{suffix}, other"),
        ] {
            let source = format!("def f(h: any); {targets} = [1, 2]; end");
            let at = source.rfind(suffix).unwrap();
            let error = Engine::new().compile(&source).err().expect(&source);
            let diagnostic = &error.diagnostics()[0];
            assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
            assert_eq!(
                diagnostic.span,
                vibescript::diagnostic::Span::new(at, at + 1)
            );
            let mut expected = source.clone();
            expected.remove(at);
            assert_eq!(
                diagnostic.applicable_fix().unwrap().apply(&source).unwrap(),
                expected
            );
        }
    }
}

#[test]
fn every_setter_declaration_rejects_method_suffixes() {
    for suffix in ['?', '!'] {
        for member in [
            format!("def ready{suffix} = (value: int); end"),
            format!("def self.ready{suffix} = (value: int); end"),
            format!("property ready{suffix}: int"),
            format!("setter ready{suffix}: int"),
        ] {
            let source = format!("class C; {member}; end");
            let at = source.find(suffix).unwrap();
            let error = Engine::new().compile(&source).err().expect(&source);
            let diagnostic = &error.diagnostics()[0];
            assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
            assert_eq!(
                diagnostic.span,
                vibescript::diagnostic::Span::new(at, at + 1)
            );
            let mut fixed = source.clone();
            fixed.remove(at);
            assert_eq!(
                diagnostic.applicable_fix().unwrap().apply(&source).unwrap(),
                fixed
            );
            Engine::new().compile(&fixed).unwrap();
        }
    }
}

#[test]
fn assignment_receivers_can_still_call_suffixed_methods() {
    for source in [
        "h = { ready?: 0 }; h['ready?'] = 2",
        "class C; property value: int; end; def box? -> C; C.new; end; box?.value = 2",
        "class C; def items! -> array<int>; [1]; end; end; C.new.items![0] = 2",
        "class C; property value: int; end; c=C.new; c.value = 1; c.value += 1",
    ] {
        assert_eq!(run(source), "2", "{source}");
    }
}

#[test]
fn question_equals_requires_an_adjacent_name_for_a_suffix_fix() {
    for source in [
        "1?=2",
        "(a + b)?=c",
        "'a'?=2",
        "a[0]?=2",
        "f()?=2",
        "true?=2",
        "a ?=2",
        "?=2",
    ] {
        let error = Engine::new().compile(source).err().expect(source);
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        for diagnostic in error.diagnostics() {
            assert_ne!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
            assert!(
                diagnostic.applicable_fix().is_none(),
                "{source}: {diagnostic:?}"
            );
        }
    }
    for source in [
        "a?=2",
        "é?=2",
        "@a?=2",
        "@@a?=2",
        "obj.a?=2",
        "obj.nil?=2",
        "obj&.true?=2",
    ] {
        let error = Engine::new().compile(source).err().expect(source);
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert_eq!(
            diagnostic.applicable_fix().unwrap().apply(source).unwrap(),
            source.replace('?', "")
        );
    }
}

#[test]
fn host_global_names_have_no_suffix() {
    for name in ["ready?", "done!", "READY?"] {
        let error = Engine::new().declare_global(name, "bool").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Syntax);
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX);
        assert_eq!(
            diagnostic.applicable_fix().unwrap().apply(name).unwrap(),
            &name[..name.len() - 1]
        );
    }
    let mut engine = Engine::new();
    engine.declare_global("left", "int").unwrap();
    engine.declare_global("right", "int").unwrap();
    let result = engine
        .compile("left!=right")
        .unwrap()
        .run(CallOptions {
            globals: [
                ("left".into(), Value::int(1)),
                ("right".into(), Value::int(2)),
            ]
            .into(),
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(
        stringify_json(&result.value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes(),
        Some(b"true".as_slice())
    );
}

#[test]
fn recovers_and_preserves_utf8_suffix_fixes() {
    let source = "é! = 3\nREADY? = 1\n@done? = true\nh.réady? = 1\nh&.ready! += 2\n";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.diagnostics().len(), 5);
    for diagnostic in error.diagnostics() {
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX);
        assert!(diagnostic.applicable_fix().unwrap().apply(source).is_some());
    }
}

fn method() -> HostMethod {
    HostMethod::new("namespace.method (diagnostic label)", |_, _, _| {
        Ok(Value::boolean(true))
    })
}

#[test]
fn huge_callable_keys_exhaust_the_execution_budget() {
    let key = vec![b'x'; 1 << 20];
    let mut exporting = Engine::new();
    exporting
        .set_module_sources([("methods.vibe".into(), "def ok? -> bool; true; end".into())].into())
        .unwrap();
    let exported = exporting
        .compile("require('methods')")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let function = exported.as_hash().unwrap()[0].1.clone();
    for callable in [method().value(), function] {
        let object = Value::object(vec![(key.clone(), callable)]);
        for route in ["global", "factory", "host return"] {
            let mut engine = Engine::new();
            let mut options = CallOptions {
                limits: Limits {
                    steps: Some(1024),
                    memory_bytes: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            let source = match route {
                "global" => {
                    engine.declare_global("cap", "").unwrap();
                    options.globals.insert("cap".into(), object.clone());
                    "cap"
                }
                "factory" => {
                    let object = object.clone();
                    let cap = Capability::new("cap", move |_| Ok(object.clone()));
                    engine.declare_capability(&cap).unwrap();
                    options.capabilities.push(cap);
                    "cap"
                }
                _ => {
                    let object = object.clone();
                    engine.register_method(
                        "supply",
                        HostMethod::new("supply", move |_, _, _| Ok(object.clone())),
                    );
                    "supply()"
                }
            };
            let script = engine.compile(source).unwrap();
            let error = script
                .run(options.clone())
                .err()
                .unwrap_or_else(|| panic!("{route} scanned a huge callable key within 1024 steps"));
            assert_eq!(error.kind, ErrorKind::Steps, "{route}: {error}");
            options.limits.steps = None;
            assert!(script.run(options).is_ok(), "{route}");
        }
    }
}

#[test]
fn registered_callable_names_use_the_compilation_budget() {
    let mut engine = Engine::new();
    engine.register_method("x".repeat(1 << 20), method());
    let options = CallOptions {
        limits: Limits {
            steps: Some(1024),
            memory_bytes: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let error = engine.compile_with_options("nil", &options).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Steps);
    assert!(engine.compile("nil").is_ok());
}

#[test]
fn callable_publication_charges_before_copying_the_key() {
    let key = vec![b'x'; 1 << 20];
    let size = key.len();
    let install = HostMethod::new_with_block("cap.install", move |call, _, _| {
        call.set_receiver_field(&key, &method().value())?;
        Ok(Value::nil())
    });
    let cap = Capability::from_value(
        "cap",
        Value::object(vec![(b"install".to_vec(), install.value())]),
    );
    let mut engine = Engine::new();
    engine.declare_capability(&cap).unwrap();
    let options = CallOptions {
        capabilities: vec![cap],
        limits: Limits {
            steps: Some(1024),
            memory_bytes: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let (result, stats) =
        engine
            .compile("cap.install()")
            .unwrap()
            .call_with_stats("__main__", &[], options);
    assert_eq!(result.unwrap_err().kind, ErrorKind::Steps);
    assert!(
        stats.peak_memory_bytes < size,
        "the key was copied before its scan was charged"
    );
}

fn suffix_error(error: Error, name: &str) {
    assert_eq!(error.kind, ErrorKind::Syntax, "{name}: {error}");
    let diagnostic = &error.diagnostics()[0];
    assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{name}: {error}");
    assert!(diagnostic.applicable_fix().unwrap().apply(name).is_some());
}

#[test]
fn registered_methods_validate_their_published_spelling() {
    for name in [
        "ok?", "save!", "é?", "bad??", "bad!!", "bad?!", "bad?name", "bad!name",
    ] {
        let valid = matches!(name, "ok?" | "save!" | "é?");
        for kind in 0..3 {
            let mut engine = Engine::new();
            match kind {
                0 => engine.register(name, |_, _| Ok(Value::boolean(true))),
                1 => engine.register_with_keywords(name, |_, _, _| Ok(Value::boolean(true))),
                _ => engine.register_method(name, method()),
            }
            let prelude = engine.prelude(&CallOptions::default());
            assert_eq!(prelude.contains(&format!("def {name}(")), valid, "{name}");
            if valid {
                let result = engine
                    .compile(name)
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap();
                assert_eq!(result.value.to_string(), "true");
            } else {
                suffix_error(engine.compile("1").err().unwrap(), name);
                suffix_error(engine.type_check("1").err().unwrap(), name);
                suffix_error(
                    engine
                        .check_entry_arguments("def run; end", "run", 0)
                        .err()
                        .unwrap(),
                    name,
                );
            }
        }
    }
    for name in ["", "bad name", "3bad", "@bad", "if", "f=", "!="] {
        let mut engine = Engine::new();
        engine.register_method(name, method());
        assert!(engine.compile("1").is_err(), "{name}");
    }
}

#[test]
fn capability_roots_and_members_share_method_spelling_validation() {
    let cap = Capability::from_value(
        "cap",
        Value::object(vec![(b"nil".to_vec(), method().value())]),
    );
    let mut engine = Engine::new();
    engine.declare_capability(&cap).unwrap();
    assert_eq!(
        engine
            .compile("cap.nil")
            .unwrap()
            .run(CallOptions {
                capabilities: vec![cap],
                ..CallOptions::default()
            })
            .unwrap()
            .value
            .to_string(),
        "true"
    );
    for name in ["ok?", "save!", "bad??", "bad!!", "bad?name", "bad!name"] {
        let valid = matches!(name, "ok?" | "save!");
        for nested in [false, true] {
            let value = if nested {
                Value::object(vec![(name.as_bytes().to_vec(), method().value())])
            } else {
                method().value()
            };
            let root = if nested { "cap" } else { name };
            let cap = Capability::from_value(root, value.clone());
            let options = CallOptions {
                capabilities: vec![cap.clone()],
                ..CallOptions::default()
            };
            let mut engine = Engine::new();
            let declaration = engine.declare_capability(&cap);
            if valid {
                declaration.unwrap();
                let source = if nested {
                    format!("cap.{name}")
                } else {
                    name.to_owned()
                };
                let script = engine.compile(&source).unwrap();
                for options in [
                    options,
                    CallOptions {
                        capabilities: vec![Capability::new(root, move |_| Ok(value.clone()))],
                        ..CallOptions::default()
                    },
                ] {
                    assert_eq!(script.run(options).unwrap().value.to_string(), "true");
                }
            } else {
                suffix_error(declaration.unwrap_err(), name);
                assert!(!engine.prelude(&options).contains(name));
            }
        }
        if !valid {
            let nested = Value::object(vec![(
                b"inner".to_vec(),
                Value::array(vec![Value::object(vec![(
                    name.as_bytes().to_vec(),
                    method().value(),
                )])]),
            )]);
            let cap = Capability::from_value("cap", nested);
            suffix_error(Engine::new().declare_capability(&cap).unwrap_err(), name);
        }
    }
}

#[test]
fn suffixed_factory_roots_are_checked_against_the_bound_value() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    for name in ["ready?", "save!"] {
        for callable in [true, false] {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let cap = Capability::new(name, move |_| {
                seen.fetch_add(1, Ordering::Relaxed);
                Ok(if callable {
                    method().value()
                } else {
                    Value::boolean(true)
                })
            });
            let options = CallOptions {
                capabilities: vec![cap.clone()],
                ..CallOptions::default()
            };
            let mut engine = Engine::new();
            let granted_prelude = engine.prelude(&options);
            assert!(granted_prelude.contains(&format!("def {name}(")));
            assert!(vibescript::signatures::Table::parse(&granted_prelude).is_ok());
            engine.declare_capability(&cap).unwrap();
            let declared_prelude = engine.prelude(&CallOptions::default());
            assert!(declared_prelude.contains(&format!("def {name}(")));
            assert!(vibescript::signatures::Table::parse(&declared_prelude).is_ok());
            assert_eq!(calls.load(Ordering::Relaxed), 0);
            for source in [name.to_owned(), format!("{name}()")] {
                let script = engine.compile(&source).unwrap();
                let before = calls.load(Ordering::Relaxed);
                let result = script.run(options.clone());
                assert_eq!(calls.load(Ordering::Relaxed), before + 1);
                if callable {
                    assert_eq!(result.unwrap().value.to_string(), "true");
                } else {
                    suffix_error(result.unwrap_err(), name);
                }
            }
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn factory_roots_reject_impossible_suffix_spellings_without_running() {
    for name in ["bad??", "bad!!", "bad?!", "bad?name", "bad!name"] {
        let cap = Capability::new(name, |_| panic!("invalid factory ran"));
        let options = CallOptions {
            capabilities: vec![cap.clone()],
            ..CallOptions::default()
        };
        let mut engine = Engine::new();
        suffix_error(engine.declare_capability(&cap).unwrap_err(), name);
        assert!(!engine.prelude(&options).contains(name));
    }
}

#[test]
fn factories_globals_and_host_publication_reject_uncallable_members() {
    for name in ["ready?", "save!", "bad??", "bad!name"] {
        let valid = matches!(name, "ready?" | "save!");
        let object = Value::object(vec![(name.as_bytes().to_vec(), method().value())]);
        let mut engine = Engine::new();
        engine.declare_global("cap", "").unwrap();
        let script = engine.compile("cap").unwrap();
        let result = script.run(CallOptions {
            globals: [("cap".into(), object.clone())].into(),
            ..CallOptions::default()
        });
        if valid {
            result.unwrap();
        } else {
            suffix_error(result.unwrap_err(), name);
        }

        let mut engine = Engine::new();
        let cap = Capability::new("cap", move |_| Ok(object.clone()));
        engine.declare_capability(&cap).unwrap();
        let result = engine.compile("1").unwrap().run(CallOptions {
            capabilities: vec![cap],
            ..CallOptions::default()
        });
        if valid {
            result.unwrap();
        } else {
            suffix_error(result.unwrap_err(), name);
        }

        let install = HostMethod::new_with_block("cap.install", move |call, _, _| {
            call.set_receiver_field(name.as_bytes(), &method().value())?;
            Ok(Value::boolean(true))
        });
        let cap = Capability::from_value(
            "cap",
            Value::object(vec![(b"install".to_vec(), install.value())]),
        );
        let mut engine = Engine::new();
        engine.declare_capability(&cap).unwrap();
        let result = engine.compile("cap.install").unwrap().run(CallOptions {
            capabilities: vec![cap],
            ..CallOptions::default()
        });
        if valid {
            assert_eq!(result.unwrap().value.to_string(), "true");
        } else {
            suffix_error(result.unwrap_err(), name);
        }
    }
    let mut engine = Engine::new();
    engine.declare_global("cap", "").unwrap();
    let script = engine.compile("cap").unwrap();
    let data = Value::object(vec![(b"bad??".to_vec(), Value::int(1))]);
    script
        .run(CallOptions {
            globals: [("cap".into(), data)].into(),
            ..CallOptions::default()
        })
        .unwrap();
}

#[test]
fn callable_globals_validate_the_binding_name() {
    let script = Engine::new().compile("1").unwrap();
    for name in ["bad??", "bad!!", "bad?name"] {
        let options = CallOptions {
            globals: [(name.into(), method().value())].into(),
            ..CallOptions::default()
        };
        assert!(!Engine::new().prelude(&options).contains(name));
        suffix_error(script.run(options).unwrap_err(), name);
    }
    for name in ["ok?", "save!"] {
        let mut engine = Engine::new();
        engine
            .declare_capability(&Capability::from_value(name, method().value()))
            .unwrap();
        let result = engine
            .compile(name)
            .unwrap()
            .run(CallOptions {
                globals: [(name.into(), method().value())].into(),
                ..CallOptions::default()
            })
            .unwrap();
        assert_eq!(result.value.to_string(), "true");
    }
}

#[test]
fn aliases_validate_bare_and_symbol_method_spellings() {
    for name in ["bad??", "bad!!", "bad?!", "bad?name", "bad!name", "é??"] {
        for alias in [
            format!("alias {name} ok"),
            format!("alias :{name} :ok"),
            format!("alias :\"{name}\" :ok"),
            format!("alias_method :{name}, :ok"),
            format!("alias_method(:\"{name}\", :ok)"),
            format!("alias good {name}"),
            format!("alias :good :{name}"),
            format!("alias_method :good, :\"{name}\""),
        ] {
            let source = format!("class C; def ok -> bool; true; end; {alias}; end");
            let error = Engine::new().compile(&source).err().unwrap();
            let diagnostic = &error.diagnostics()[0];
            assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
            let fixed = diagnostic.applicable_fix().unwrap().apply(&source).unwrap();
            if let Err(error) = Engine::new().compile(&fixed) {
                assert_ne!(
                    error.diagnostics().first().map(|d| d.code),
                    Some(Code::NAME_SUFFIX),
                    "{fixed}: {error}"
                );
            }
        }
    }
    for (source, fixed) in [
        (
            r#"class C; def ok -> bool; true; end; alias_method :"bad\x3f?", :ok; end"#,
            r#"class C; def ok -> bool; true; end; alias_method :"bad?", :ok; end"#,
        ),
        ("def ok? = (v: bool); end", "def ok = (v: bool); end"),
        (
            "class C; def self.ok? = (v: bool); end; end",
            "class C; def self.ok = (v: bool); end; end",
        ),
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert_eq!(
            diagnostic.applicable_fix().unwrap().apply(source).unwrap(),
            fixed
        );
        if let Err(error) = Engine::new().compile(fixed) {
            assert_ne!(error.kind, ErrorKind::Syntax, "{fixed}: {error}");
        }
    }
}

#[test]
fn aliases_preserve_suffixes_operators_and_setters() {
    for alias in [
        "alias ready? ok",
        "alias :ready? :ok",
        "alias :\"ready?\" :ok",
        "alias_method :ready?, :ok",
        "alias_method(:\"ready?\", :ok)",
    ] {
        assert_eq!(
            run(&format!(
                "class C; def ok -> bool; true; end; {alias}; end; C.new.ready?"
            )),
            "true"
        );
    }
    for source in [
        "class C; def ok! -> bool; true; end; alias_method :ready?, :ok!; end; C.new.ready?",
        "class C; def ok(n: int) -> bool; n == 1; end; alias_method :!=, :ok; end; C.new != 1",
        "class C; def ok(n: int) -> bool; n == 1; end; alias :\"!=\" :ok; end; C.new != 1",
        "class C; def ok(n: int) -> bool; n == 1; end; alias_method :[], :ok; end; C.new[1]",
        "class C; property x: int; alias_method :\"y=\", :\"x=\"; end; c = C.new; c.y = 1; c.x == 1",
    ] {
        assert_eq!(run(source), "true", "{source}");
    }
}

#[test]
fn aliases_name_every_operator_an_instance_dispatches() {
    // Operators without a symbol token are spelled as quoted symbols.
    for (op, symbol) in [
        ("+", true),
        ("-", true),
        ("*", true),
        ("/", true),
        ("//", false),
        ("%", true),
        ("**", true),
        ("&", true),
        ("==", true),
        ("!=", true),
        ("===", true),
        ("=~", false),
        ("!~", false),
        ("<", true),
        ("<=", true),
        (">", true),
        (">=", true),
        ("<=>", true),
    ] {
        let mut aliases = vec![
            format!("alias :\"{op}\" :ok"),
            format!("alias_method :\"{op}\", :ok"),
        ];
        if symbol {
            aliases.push(format!("alias :{op} :ok"));
            aliases.push(format!("alias_method :{op}, :ok"));
        }
        for alias in aliases {
            let source =
                format!("class C; def ok(n: int) -> bool; n == 2; end; {alias}; end; C.new {op} 2");
            assert_eq!(run(&source), "true", "{source}");
        }
    }
    for source in [
        "class C; def ok(n: int) -> int; n + 3; end; alias :<< :ok; end; C.new << 2",
        "class C; def ok(n: int) -> int; n + 3; end; alias :[] :ok; end; C.new[2]",
        "class C; @last: int = 0; def store(n: int, v: int) -> int; @last = n + v; end; alias_method :[]=, :store; def last -> int; @last; end; end; c = C.new; c[2] = 3; c.last",
    ] {
        assert_eq!(run(source), "5", "{source}");
    }
}

#[test]
fn aliases_reject_operators_that_never_dispatch() {
    for op in ["!", "&&", "||", "|"] {
        for alias in [
            format!("alias :{op} :ok"),
            format!("alias :\"{op}\" :ok"),
            format!("alias_method :{op}, :ok"),
            format!("alias_method(:\"{op}\", :ok)"),
            format!("alias :good :{op}"),
            format!("private :{op}"),
        ] {
            let source = format!("class C; def ok -> bool; true; end; {alias}; end");
            let error = Engine::new().compile(&source).err().expect(&source);
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
            assert!(
                error
                    .message
                    .starts_with(&format!("`{op}` cannot name a method")),
                "{source}: {error}"
            );
            for diagnostic in error.diagnostics() {
                assert_ne!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
                assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
            }
        }
    }
}

#[test]
fn exported_functions_share_method_spelling_validation() {
    for name in ["ok?", "save!", "bad??", "bad!name"] {
        let source = format!("def {name} -> bool; true; end");
        let mut engine = Engine::new();
        engine
            .set_module_sources([("methods.vibe".into(), source)].into())
            .unwrap();
        if matches!(name, "ok?" | "save!") {
            let result = engine
                .compile(&format!("require('methods').{name}"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap();
            assert_eq!(result.value.to_string(), "true");
            assert_eq!(
                run(&format!(
                    "module M; def self.{name} -> bool; true; end; end; M.{name}"
                )),
                "true"
            );
        } else {
            let error = engine.compile("require('methods')").err().unwrap();
            assert!(
                error.message.contains("only method names may end"),
                "{error}"
            );
            let error = Engine::new()
                .compile(&format!("module M; def self.{name}; end; end"))
                .err()
                .unwrap();
            assert_eq!(error.diagnostics()[0].code, Code::NAME_SUFFIX);
        }
    }
}
