use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Value, diagnostic::Code,
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
    let source = "é! = 3\nREADY? = 1\n@done? = true\n";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.diagnostics().len(), 3);
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
