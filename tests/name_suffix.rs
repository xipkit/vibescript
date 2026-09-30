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
fn adjacent_negated_match_never_ends_a_name() {
    for source in [
        "s = 'abc'; [s!~/z/, s!~/b/] == [true, false]",
        "S = 'abc'; S!~/z/",
        "class C; @s: string = 'a'; def f -> bool; @s!~/z/; end; end; C.new.f",
        "class C; @@s: string = 'a'; def self.f -> bool; @@s!~/z/; end; end; C.f",
        "def ok? -> string; 'abc'; end; ok?!~/z/",
        "def save! -> string; 'abc'; end; save!!~/z/",
        "def text! -> string; 'yes'; end; (text!=~/yes/) == 0",
    ] {
        assert_eq!(run(source), "true", "{source}");
    }
}

#[test]
fn a_method_read_before_a_comma_keeps_one_suffix() {
    // Only a name that starts a statement can be a destructuring target.
    for (source, fixed) in [
        ("p([1].first??, 2)", "p([1].first?, 2)"),
        (
            "h = { a: \"\".empty!?, b: 1 }",
            "h = { a: \"\".empty?, b: 1 }",
        ),
        ("x??, y = [1, 2]", "x, y = [1, 2]"),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
    }
}

#[test]
fn nested_destructuring_targets_are_bindings() {
    for (source, fixed) in [
        (
            "a, [x??, y] = [1, [2, 3]]; x??",
            "a, [x, y] = [1, [2, 3]]; x",
        ),
        (
            "a, (b!?, c) = [1, [2, 3]]; b!?",
            "a, (b, c) = [1, [2, 3]]; b",
        ),
        (
            "for a??, b in [[1, 2]]; p(a??); end",
            "for a, b in [[1, 2]]; p(a); end",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        Engine::new().type_check(fixed).unwrap();
    }
    // A name read inside a target, as an index is, is no target itself.
    assert_eq!(
        first_fix("h = { a: 1 }; a, [h[k??], y] = [1, [2, 3]]"),
        "h = { a: 1 }; a, [h[k?], y] = [1, [2, 3]]"
    );
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
fn optional_type_defaults_keep_removed_spelling_diagnostics() {
    for (source, code) in [
        (
            "def f(x: int?=nil) -> bool\n  y = 1\n  y.nil?\nend\nf",
            Code::NIL_PREDICATE,
        ),
        (
            "def f x: int?=nil -> bool\n  [1].each do |n| n end\n  true\nend\nf",
            Code::DO_BLOCK,
        ),
        (
            "class C; def initialize(x: bool?=true); end; def u(y: int) -> int; y; end; end; unless false; 1; end",
            Code::UNLESS,
        ),
    ] {
        let checked = Engine::new().type_check(source).unwrap();
        assert!(
            checked.diagnostics.iter().any(|d| d.code == code),
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
        ("[1].each { |x!| x! }", "[1].each { |x| x }"),
        ("def f(ok?: bool); end", "def f(ok: bool); end"),
        (
            "def f(*ok!: array<int>); end",
            "def f(*ok: array<int>); end",
        ),
        (
            "def f(**ok?: hash<string, bool>); end",
            "def f(**ok: hash<string, bool>); end",
        ),
        ("def f(&block!: ()); end", "def f(&block: ()); end"),
        ("@done? = true", "@done = true"),
        ("@@done! = true", "@@done = true"),
        ("@done?", "@done"),
        ("@@done!", "@@done"),
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
fn suffix_fixes_keep_names_valid_where_they_stand() {
    for (source, fixed) in [
        ("nil? = 3", "nil_ = 3"),
        ("true? = 1", "true_ = 1"),
        ("false! = 1", "false_ = 1"),
        ("self? = 1", "self_ = 1"),
        ("private? = 1", "private_ = 1"),
        ("property! = 1", "property_ = 1"),
        ("getter? = 1", "getter_ = 1"),
        ("setter! = 1", "setter_ = 1"),
        ("def f(nil?: int); end", "def f(nil_: int); end"),
        ("[1].each { |then!| }", "[1].each { |then_| }"),
        (
            "class C; @nil?: int = 1; end",
            "class C; @nil: int = 1; end",
        ),
        (
            "def f(h: any); h.nil? = 2; end",
            "def f(h: any); h.nil = 2; end",
        ),
        (
            "class C; def ok!=(v: bool); end; end",
            "class C; def ok=(v: bool); end; end",
        ),
        (
            "class C; def self.ok!=(v: bool); end; end",
            "class C; def self.ok=(v: bool); end; end",
        ),
        (
            "class C; def ok?=(v: bool); end; end",
            "class C; def ok=(v: bool); end; end",
        ),
    ] {
        let error = Engine::new().compile(source).err().expect(source);
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
fn characters_inside_a_name_are_not_suffixes_to_remove() {
    for source in [
        "x1 = 5; x = true; p(x?1)",
        "ready = true; limit = 5; ready_limit = 9; x = ready?limit : 0",
        "x?1 = 5",
        "x!y, z = [1, 2]",
        "def f(a?b: int); end",
        "def fo?o; end",
        "def f(h: any); h.a?b; end",
        "alias :a?b :ok",
    ] {
        let error = Engine::new().compile(source).err().expect(source);
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
}

#[test]
fn scoped_suffixed_calls_keep_their_dot_fix() {
    for (source, fixed) in [
        (
            "module M; def self.OK? -> bool; true; end; end; M::OK?",
            "module M; def self.OK? -> bool; true; end; end; M.OK?",
        ),
        (
            "module M; def self.save! -> bool; true; end; end; M::save!",
            "module M; def self.save! -> bool; true; end; end; M.save!",
        ),
    ] {
        let checked = Engine::new().type_check(source).unwrap();
        let diagnostic = &checked.diagnostics[0];
        assert_eq!(diagnostic.code, Code::SCOPED_CALL, "{source}");
        let fix = diagnostic.applicable_fix().expect(source);
        assert_eq!(fix.apply(source).unwrap(), fixed);
        assert_eq!(run(fixed), "true");
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
        registration_error(&error, "global", name);
        assert!(
            error
                .message
                .ends_with("only method names may end in `?` or `!`")
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
        if let Some(fix) = diagnostic.applicable_fix() {
            assert!(fix.apply(source).is_some());
        }
    }
    // The last line is no assignment target even with suffixes allowed, so
    // the reads of `é!` and `READY?` after it are unknown and their bindings
    // get no fix; the instance variable and the members still do.
    let fixed: Vec<bool> = error
        .diagnostics()
        .iter()
        .map(|d| d.applicable_fix().is_some())
        .collect();
    assert_eq!(fixed, [false, false, true, true, true]);
    let source = "é! = 3\nREADY? = 1\n@done? = true\nh.réady? = 1\nh.ready! += 2\n";
    let error = Engine::new().compile(source).err().unwrap();
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|d| d.applicable_fix().is_some())
    );
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

/// Asserts a host API error that names the registration of `name`, with no
/// diagnostic or fix that could edit a script, and returns it rendered.
fn host_name_error(error: &Error, kind: &str, name: &str) -> String {
    assert_eq!(error.kind, ErrorKind::Argument, "{name}: {error}");
    assert!(
        error
            .message
            .starts_with(&format!("invalid {kind} name \"{name}\"")),
        "{name}: {error}"
    );
    assert!(error.diagnostics().is_empty(), "{name}: {error}");
    let rendered = error.to_string();
    assert!(!rendered.contains("parse error"), "{name}: {rendered}");
    rendered
}

/// Asserts a host API error reported by a registration, or by compiling
/// before any script runs, which has no position in the script.
fn registration_error(error: &Error, kind: &str, name: &str) {
    let rendered = host_name_error(error, kind, name);
    assert!(error.offset.is_none(), "{name}: {rendered}");
    assert!(error.diagnostic.is_none(), "{name}: {rendered}");
    assert_eq!(rendered, error.message);
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
                for error in [
                    engine.compile("total = 1; total").err().unwrap(),
                    engine.type_check("total = 1; total").err().unwrap(),
                    engine
                        .check_entry_arguments("def run; end", "run", 0)
                        .err()
                        .unwrap(),
                ] {
                    registration_error(&error, "host function", name);
                }
            }
        }
    }
    for name in ["", "bad name", "3bad", "@bad", "if", "f=", "!="] {
        let mut engine = Engine::new();
        engine.register_method(name, method());
        registration_error(&engine.compile("1").err().unwrap(), "host function", name);
    }
}

#[test]
fn receiverless_host_names_are_identifiers_not_operators() {
    // Nothing calls a function or a capability root without a receiver by
    // an operator, so each route that publishes one rejects the spelling.
    for name in ["+", "[]", "[]=", "<<", "=~", "==", "<=>", "-@"] {
        let mut engine = Engine::new();
        engine.register_method(name, method());
        registration_error(&engine.compile("1").err().unwrap(), "host function", name);
        let template = Capability::from_value(name, method().value());
        let error = Engine::new().declare_capability(&template).err().unwrap();
        host_name_error(&error, "capability", name);
        let factory = Capability::new(name, |_| Ok(method().value()));
        let error = Engine::new().declare_capability(&factory).err().unwrap();
        host_name_error(&error, "capability", name);
        let mut options = CallOptions::default();
        options.globals.insert(name.into(), method().value());
        let error = Engine::new()
            .compile("1")
            .unwrap()
            .run(options)
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Argument, "{name}: {error}");
        assert!(
            error
                .message
                .starts_with(&format!("invalid global name \"{name}\"")),
            "{name}: {error}"
        );
    }
    // A script's class still answers operators with methods so named.
    let source = "class V; getter n: int; def initialize(@n: int); end\n\
                  def +(other: V) -> V; V.new(@n + other.n); end\n\
                  def [](i: int) -> int; @n + i; end; end\n\
                  (V.new(1) + V.new(2))[3]";
    assert_eq!(run(source), "6");
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
                let kind = if nested { "method" } else { "capability" };
                registration_error(&declaration.unwrap_err(), kind, name);
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
            let error = Engine::new().declare_capability(&cap).unwrap_err();
            registration_error(&error, "method", name);
            assert!(
                error.message.contains(" in capability \"cap\": "),
                "{error}"
            );
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
                    let error = result.unwrap_err();
                    host_name_error(&error, "capability", name);
                    assert!(error.message.ends_with("not callable"), "{error}");
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
        registration_error(
            &engine.declare_capability(&cap).unwrap_err(),
            "capability",
            name,
        );
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
            host_name_error(&result.unwrap_err(), "method", name);
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
            host_name_error(&result.unwrap_err(), "method", name);
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
            host_name_error(&result.unwrap_err(), "method", name);
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
        host_name_error(&script.run(options).unwrap_err(), "global", name);
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
            if name.ends_with("name") {
                assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
                continue;
            }
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
                error.message.contains("may only end a method name")
                    || error.message.contains("only method names may end"),
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

#[test]
fn only_script_functions_publish_setters() {
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, args, _| Ok(args[0].clone())),
    );
    engine
        .set_module_sources(
            [(
                "m.vibe".into(),
                "def value=(v: int) -> int\n  v * 2\nend\ndef value -> int\n  1\nend\n".into(),
            )]
            .into(),
        )
        .unwrap();
    for source in [
        "echo(require('m'))",
        "m = require('m'); m.value = 3; m.value",
    ] {
        engine
            .compile(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
    }

    // No script can call a host method as a setter, so no host value may
    // publish one: not a template, a factory's value, a global or a callback.
    fn setter() -> HostMethod {
        HostMethod::new("cap.value=", |_, args, _| Ok(args[0].clone()))
    }
    fn object() -> Value {
        Value::object(vec![(b"value=".to_vec(), setter().value())])
    }
    let error = Engine::new()
        .declare_capability(&Capability::from_value("cap", object()))
        .unwrap_err();
    registration_error(&error, "method", "value=");
    assert!(
        error.message.contains(" in capability \"cap\": "),
        "{error}"
    );
    assert!(error.message.contains("setter"), "{error}");
    let mut engine = Engine::new();
    engine.declare_global("cap", "").unwrap();
    let error = engine
        .compile("cap")
        .unwrap()
        .run(CallOptions {
            globals: [("cap".into(), object())].into(),
            ..CallOptions::default()
        })
        .unwrap_err();
    host_name_error(&error, "method", "value=");
    let factory = Capability::new("cap", move |_| Ok(object()));
    let mut engine = Engine::new();
    engine.declare_capability(&factory).unwrap();
    let error = engine
        .compile("1")
        .unwrap()
        .run(CallOptions {
            capabilities: vec![factory],
            ..CallOptions::default()
        })
        .unwrap_err();
    host_name_error(&error, "method", "value=");
    let install = HostMethod::new_with_block("cap.install", |call, _, _| {
        call.set_receiver_field(b"value=", &setter().value())?;
        Ok(Value::nil())
    });
    let cap = Capability::from_value(
        "cap",
        Value::object(vec![(b"install".to_vec(), install.value())]),
    );
    let mut engine = Engine::new();
    engine.declare_capability(&cap).unwrap();
    let error = engine
        .compile("cap.install")
        .unwrap()
        .run(CallOptions {
            capabilities: vec![cap],
            ..CallOptions::default()
        })
        .unwrap_err();
    host_name_error(&error, "method", "value=");

    for name in ["value=", "ok?="] {
        let mut engine = Engine::new();
        engine.register_method(name, method());
        registration_error(&engine.compile("1").err().unwrap(), "host function", name);
    }
}

/// Applies the first applicable fix of each check until none remains, as
/// `vibes fix` does one fix at a time.
fn migrate(source: &str) -> String {
    let engine = Engine::new();
    let mut text = source.to_owned();
    for _ in 0..64 {
        let diagnostics = match engine.type_check(&text) {
            Ok(checked) => checked.diagnostics,
            Err(error) => error.diagnostics().to_vec(),
        };
        let Some(fix) = diagnostics.iter().find_map(|d| d.applicable_fix()) else {
            return text;
        };
        text = fix.apply(&text).unwrap();
    }
    panic!("{source} did not converge: {text}");
}

/// The first diagnostic's applicable fix, applied alone.
fn first_fix(source: &str) -> String {
    let Err(error) = Engine::new().type_check(source) else {
        panic!("{source} checks");
    };
    let diagnostic = &error.diagnostics()[0];
    assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
    let fix = diagnostic.applicable_fix().expect(source);
    fix.apply(source).unwrap()
}

#[test]
fn a_binding_fix_renames_every_use_of_the_binding_at_once() {
    for (source, fixed) in [
        (
            "def check(ok?: bool) -> bool\n  ok?\nend\ncheck(true)",
            "def check(ok: bool) -> bool\n  ok\nend\ncheck(true)",
        ),
        (
            "total! = 0\n[1].each { |n| total! += n }\ntotal!",
            "total = 0\n[1].each { |n| total += n }\ntotal",
        ),
        ("x?=3; x? + 1", "x=3; x + 1"),
        ("ok? = 1; \"#{ok?}\"", "ok = 1; \"#{ok}\""),
        ("n! = 2; n!.to_s", "n = 2; n.to_s"),
        (
            "list! = [1]; list!.each { |x| x }",
            "list = [1]; list.each { |x| x }",
        ),
        (
            "def f(nil?: int) -> int; nil?; end; f(1)",
            "def f(nil_: int) -> int; nil_; end; f(1)",
        ),
        ("if? = 1; if? + 1", "if_ = 1; if_ + 1"),
        (
            "begin; raise 'x'; rescue => error!; error!.message; end",
            "begin; raise 'x'; rescue => error; error.message; end",
        ),
        ("[1, 2].map { |x?| x? * 2 }", "[1, 2].map { |x| x * 2 }"),
        (
            "class C; LIMIT! = 3; def f -> int; LIMIT!; end; end; C.new.f",
            "class C; LIMIT = 3; def f -> int; LIMIT; end; end; C.new.f",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        run(fixed);
    }
    // A block parameter of the same name is renamed with the local it
    // shadows. The binding's first diagnostic carries the one fix; the
    // others carry none, which could rename only part of it.
    let source = "x? = 1\n[2].map { |x?| x? }\nx?";
    let Err(error) = Engine::new().type_check(source) else {
        panic!("{source} checks");
    };
    let diagnostics = error.diagnostics();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0]
            .applicable_fix()
            .unwrap()
            .apply(source)
            .unwrap(),
        "x = 1\n[2].map { |x| x }\nx"
    );
    assert!(diagnostics[1].fixes.is_empty(), "{:?}", diagnostics[1]);
    assert_eq!(migrate(source), "x = 1\n[2].map { |x| x }\nx");
}

#[test]
fn reads_that_no_suffixed_binding_owns_are_never_renamed() {
    // A host function the check cannot see, as in a host-less `vibes fix`.
    let host = "def run(ready: bool) -> string\n  if ready?\n    \"go\"\n  else\n    \"wait\"\n  end\nend\n";
    for source in [
        host,
        "ok = 1; ok?",
        "ok = 1; ok?(2)",
        "x = 1; def f -> int; x?; end",
        "p(ok?); ok = 1",
    ] {
        let checked = Engine::new().type_check(source).unwrap();
        let undefined = checked
            .diagnostics
            .iter()
            .find(|d| d.code == Code::UNDEFINED_NAME)
            .unwrap_or_else(|| panic!("{source}: {:?}", checked.diagnostics));
        assert!(undefined.fixes.is_empty(), "{source}: {undefined:?}");
        assert_eq!(migrate(source), source);
    }
    // The shorthand label calls the method `ready?`, so the key stays.
    assert_eq!(
        first_fix("ready? = true; { ready?: 1 }"),
        "ready = true; { ready?: 1 }"
    );
}

#[test]
fn constant_fixes_rename_scoped_reads_of_their_namespace() {
    for (source, fixed, value) in [
        (
            "class C; LIMIT! = 3; end; C::LIMIT!",
            "class C; LIMIT = 3; end; C::LIMIT",
            "3",
        ),
        (
            "module M; READY? = true; end; M::READY?",
            "module M; READY = true; end; M::READY",
            "true",
        ),
        (
            "enum State; Ready?; end; State::Ready? == State::Ready?",
            "enum State; Ready; end; State::Ready == State::Ready",
            "true",
        ),
        (
            "class C; LIMIT! = 3; def f -> int; C::LIMIT! + LIMIT!; end; end; C.new.f",
            "class C; LIMIT = 3; def f -> int; C::LIMIT + LIMIT; end; end; C.new.f",
            "6",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        assert_eq!(migrate(source), fixed, "{source}");
        assert_eq!(run(fixed), value, "{fixed}");
    }
    // Only the declaring namespace's scoped reads are renamed.
    let source = "class C; LIMIT! = 3; end\nclass D; def self.LIMIT! -> int; 4; end; end\n[C::LIMIT!, D.LIMIT!]";
    assert_eq!(
        first_fix(source),
        "class C; LIMIT = 3; end\nclass D; def self.LIMIT! -> int; 4; end; end\n[C::LIMIT, D.LIMIT!]"
    );
    // A reopened class binds the same constant, renamed in every body.
    assert_eq!(
        first_fix("class C; X! = 1; end; class C; X! = 2; end; C::X!"),
        "class C; X = 1; end; class C; X = 2; end; C::X"
    );
    // A scope the parse cannot resolve, such as a local holding the class,
    // may reach the member, so there is no fix rather than a partial one.
    for source in [
        "class C; LIMIT! = 3; end; c = C; [c::LIMIT!, C::LIMIT!]",
        "class C; LIMIT! = 3; end; [[C][0]::LIMIT!, C::LIMIT!]",
        "class A; class B!; end; end; a = A; [a::B!.new, A::B!.new].length",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
}

#[test]
fn scoped_reads_resolve_the_whole_namespace_path() {
    for (source, fixed) in [
        // Same-named namespaces in different parents: only `A::M`'s
        // constant is renamed, not `B::M`'s method of the same name.
        (
            "module A; module M; READY? = 1; end; end\nmodule B; module M; def self.READY? -> int; 2; end; end; end\n[A::M::READY?, B::M::READY?]",
            "module A; module M; READY = 1; end; end\nmodule B; module M; def self.READY? -> int; 2; end; end; end\n[A::M::READY, B::M::READY?]",
        ),
        // Nested modules, read through the full path and from the parent.
        (
            "module A; module M; X! = 1; end; def self.f -> int; M::X!; end; end\n[A::M::X!, A.f]",
            "module A; module M; X = 1; end; def self.f -> int; M::X; end; end\n[A::M::X, A.f]",
        ),
        // A relative reference resolves inside the reading namespace first,
        // so the top-level `M` is not `A::M`.
        (
            "module M; X! = 2; end\nmodule A; module M; X! = 1; end; def self.f -> int; M::X!; end; end\n[M::X!, A.f]",
            "module M; X = 2; end\nmodule A; module M; X! = 1; end; def self.f -> int; M::X!; end; end\n[M::X, A.f]",
        ),
        // A sibling's constant, reached through the enclosing namespace.
        (
            "module A; module M; X? = 1; end; module N; def self.f -> int; M::X?; end; end; end\nA::M::X?",
            "module A; module M; X = 1; end; module N; def self.f -> int; M::X; end; end; end\nA::M::X",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
    }
    // Once the top-level `M`'s constant is `X`, renaming the inner one's
    // would spell a name the file already spells, so it keeps no fix.
    let source = "module M; X! = 2; end\nmodule A; module M; X! = 1; end; def self.f -> int; M::X!; end; end\n[M::X!, A.f]";
    assert_eq!(
        migrate(source),
        "module M; X = 2; end\nmodule A; module M; X! = 1; end; def self.f -> int; M::X!; end; end\n[M::X, A.f]"
    );
}

#[test]
fn suffix_fixes_leave_a_valid_name_or_are_not_offered() {
    // Without the suffix nothing names the variable, so no fix is offered.
    for source in [
        "@? = 1",
        "@@! = 1",
        "@?? = 1",
        "@@!? = 1",
        "class C; @?: int = 1; end",
        "class C; @@!: int = 1; end",
        // Without the `?` these would be assignments where none can stand.
        "x = { a: ok?=(1) }",
        "p(ok?=(1))",
        "x = [ok?=(1)]",
        "if true && ok?=(1); end",
    ] {
        let Err(error) = Engine::new().compile(source) else {
            panic!("{source} compiles");
        };
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
    for (source, fixed) in [
        // A binding loses its whole run of suffix characters.
        ("x?? = 1", "x = 1"),
        ("x?! = 1", "x = 1"),
        ("x!? = 1", "x = 1"),
        ("x??? = 1; x???", "x = 1; x"),
        ("nil?? = 1", "nil_ = 1"),
        ("@x?? = 1", "@x = 1"),
        ("@@x!? = 1", "@@x = 1"),
        ("def f(ok?!: bool); end", "def f(ok: bool); end"),
        ("[1].each { |x??| x?? }", "[1].each { |x| x }"),
        ("for x?! in [1]; end", "for x in [1]; end"),
        ("x??, y = [1, 2]", "x, y = [1, 2]"),
        (
            "def f(h: any); h.ready?? = 1; end",
            "def f(h: any); h.ready = 1; end",
        ),
        (
            "class C; def ok?!=(v: int); end; end",
            "class C; def ok=(v: int); end; end",
        ),
        (
            "class C; def nil?!=(v: int); end; end",
            "class C; def nil_=(v: int); end; end",
        ),
        // A method keeps the run's last character.
        ("def bad??; end", "def bad?; end"),
        ("def bad?!; end", "def bad!; end"),
        ("def bad!?!; end", "def bad!; end"),
        (
            "class C; def ok -> bool; true; end; alias :bad?? :ok; end",
            "class C; def ok -> bool; true; end; alias :bad? :ok; end",
        ),
        (
            "class C; def ok -> bool; true; end; alias :\"bad???\" :ok; end",
            "class C; def ok -> bool; true; end; alias :\"bad?\" :ok; end",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        if let Err(error) = Engine::new().compile(fixed) {
            assert_ne!(error.kind, ErrorKind::Syntax, "{fixed}: {error}");
        }
    }
}

#[test]
fn exported_functions_declare_callable_capability_members() {
    let mut exporting = Engine::new();
    exporting
        .set_module_sources([("m.vibe".into(), "def ok? -> bool; true; end".into())].into())
        .unwrap();
    let function = exporting
        .compile("require('m')")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
        .as_hash()
        .unwrap()[0]
        .1
        .clone();
    // An object whose only callable fields are exported functions is a
    // namespace the checker calls, as the runtime does.
    for fields in [
        vec![(b"run?".to_vec(), function.clone())],
        vec![(
            b"inner".to_vec(),
            Value::object(vec![(b"run?".to_vec(), function.clone())]),
        )],
    ] {
        let nested = fields[0].0 == b"inner";
        let cap = Capability::from_value("cap", Value::object(fields));
        let script = if nested { "cap.inner.run?" } else { "cap.run?" };
        let mut engine = Engine::new();
        engine.declare_capability(&cap).unwrap();
        let checked = engine.type_check(script).unwrap();
        assert!(
            checked.diagnostics.is_empty(),
            "{script}: {:?}",
            checked.diagnostics
        );
        let options = CallOptions {
            capabilities: vec![cap],
            ..CallOptions::default()
        };
        let value = engine.compile(script).unwrap().run(options).unwrap().value;
        assert_eq!(value.to_string(), "true", "{script}");
    }
    // A root that is one binds as no value a script can use, so the checker
    // does not declare it callable either.
    let cap = Capability::from_value("ready?", function);
    let mut engine = Engine::new();
    engine.declare_capability(&cap).unwrap();
    let error = engine.compile("ready?()").err().unwrap();
    assert!(error.message.contains("not a function"), "{error}");
}

#[test]
fn ordinary_hashes_validate_their_callable_fields() {
    // A hash exposes its fields as an object does, so a callable field must
    // spell a method; its data keys may hold any punctuation.
    let hash = |name: &str| {
        Value::hash(vec![
            (name.as_bytes().to_vec(), method().value()),
            (b"data??".to_vec(), Value::int(1)),
        ])
    };
    let mut exporting = Engine::new();
    exporting
        .set_module_sources([("m.vibe".into(), "def ok? -> bool; true; end".into())].into())
        .unwrap();
    let function = exporting
        .compile("require('m')")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
        .as_hash()
        .unwrap()[0]
        .1
        .clone();
    let with_function =
        |name: &str| Value::hash(vec![(name.as_bytes().to_vec(), function.clone())]);
    for (name, valid) in [("ok?", true), ("bad??", false), ("bad?name", false)] {
        for value in [hash(name), with_function(name)] {
            for route in ["global", "factory", "host return"] {
                let mut engine = Engine::new();
                let mut options = CallOptions::default();
                let value = value.clone();
                match route {
                    "global" => {
                        engine.declare_global("cap", "").unwrap();
                        options.globals.insert("cap".into(), value);
                    }
                    "factory" => {
                        let cap = Capability::new("cap", move |_| Ok(value.clone()));
                        engine.declare_capability(&cap).unwrap();
                        options.capabilities.push(cap);
                    }
                    _ => engine.register_method(
                        "cap",
                        HostMethod::new("cap", move |_, _, _| Ok(value.clone())),
                    ),
                }
                let result = engine.compile("cap\n1").unwrap().run(options);
                if valid {
                    // The import accepts the field; a script may still not
                    // use a hash holding a method as a value.
                    if let Err(error) = result {
                        assert!(!error.message.starts_with("invalid"), "{route}: {error}");
                    }
                } else {
                    host_name_error(&result.unwrap_err(), "method", name);
                }
            }
        }
    }
}

/// Applies every fix that edits no other's text, round by round, as
/// `vibes fix` does.
fn fix_all(source: &str) -> String {
    let engine = Engine::new();
    let mut text = source.to_owned();
    for _ in 0..64 {
        let diagnostics = match engine.type_check(&text) {
            Ok(checked) => checked.diagnostics,
            Err(error) => error.diagnostics().to_vec(),
        };
        let mut edits = Vec::new();
        for fix in diagnostics.iter().filter_map(|d| d.applicable_fix()) {
            let overlaps = fix.edits.iter().any(|edit| {
                edits.iter().any(|other: &vibescript::diagnostic::Edit| {
                    edit.span.start < other.span.end && other.span.start < edit.span.end
                })
            });
            if !overlaps {
                edits.extend(fix.edits.iter().cloned());
            }
        }
        if edits.is_empty() {
            return text;
        }
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
        for edit in edits {
            text.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
    }
    panic!("{source} keeps changing");
}

#[test]
fn every_spelling_of_one_name_may_take_the_same_fix() {
    // A method's name, and a variable's with a sigil, is fixed where it is
    // spelled, each V0003 on its own, so the fixes may leave the same name.
    for (source, fixed) in [
        (
            "def ok??; 1; end; def g; ok??; end; ok??",
            "def ok?; 1; end; def g; ok?; end; ok?",
        ),
        (
            "class C; def initialize(@d?: bool); end; def g -> bool; @d?; end; end",
            "class C; def initialize(@d: bool); end; def g -> bool; @d; end; end",
        ),
        (
            "class C; def v?=(x: int); end; end; C.new.v? = 1",
            "class C; def v=(x: int); end; end; C.new.v = 1",
        ),
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        for diagnostic in error.diagnostics() {
            assert!(
                diagnostic.applicable_fix().is_some(),
                "{source}: {diagnostic:?}"
            );
        }
        assert_eq!(fix_all(source), fixed, "{source}");
    }
}

#[test]
fn a_fix_of_one_place_waits_for_every_place_its_spelling_is_written() {
    // An accessor's calls are member calls, valid with the suffix and with
    // no fix of their own, so renaming its declaration alone would leave
    // them calling a method that no longer exists.
    for source in [
        "class A; property done?: bool; end; A.new.done?",
        "class A; getter done?: bool; end; A.new.done?",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
    assert_eq!(
        first_fix("class A; property done?: bool; end"),
        "class A; property done: bool; end"
    );
}

#[test]
fn every_form_that_names_something_is_fixed_whole_or_not_at_all() {
    // Each form that introduces a name: `true` where `vibes fix` renames it
    // everywhere, `false` where its V0003 keeps no fix.
    for (form, source, renamed) in [
        ("local", "x? = 1; x?", true),
        ("destructuring", "x?, y = [1, 2]; x?", true),
        (
            "nested destructuring",
            "a, [x??, y] = [1, [2, 3]]; x??",
            true,
        ),
        ("rest target", "a, *r? = [1, 2]; r?", true),
        ("compound", "x? = 1; x? += 1; x?", true),
        ("typed local", "x?: int = 1; x?", true),
        ("parameter", "def f(a?: int) -> int; a?; end; f(1)", true),
        ("default", "def f(a?: int = 1) -> int; a?; end; f", true),
        (
            "keyword parameter",
            "def f(*, k?: int = 1) -> int; k?; end; f(k?: 2)",
            false,
        ),
        (
            "rest parameter",
            "def f(*r?: array<int>) -> int; r?.length; end; f(1)",
            true,
        ),
        (
            "keyword rest",
            "def f(**o?: hash<string, int>) -> int; o?.length; end; f",
            true,
        ),
        ("block parameter", "[1].map { |x?| x? }", true),
        ("block destructuring", "[[1, 2]].map { |(a?, b)| a? }", true),
        ("for", "for x? in [1]; p(x?); end", true),
        (
            "for destructuring",
            "for a??, b in [[1, 2]]; p(a??); end",
            true,
        ),
        ("rescue", "begin; 1; rescue => e?; e?; end", true),
        ("method", "def ok??; 1; end; def g; ok??; end; ok??", true),
        (
            "setter",
            "class C; def v?=(x: int); end; end; C.new.v? = 1",
            true,
        ),
        (
            "alias",
            "class C; def ok; end; alias ok?? ok; end; C.new.ok??",
            true,
        ),
        (
            "property",
            "class A; property done?: bool; end; A.new.done?",
            false,
        ),
        (
            "getter",
            "class A; getter done?: bool; end; A.new.done?",
            false,
        ),
        (
            "instance variable",
            "class C; @d?: int = 1; def g -> int; @d?; end; end",
            true,
        ),
        (
            "parameter's instance variable",
            "class C; @d?: bool; def initialize(@d?: bool); end; def g -> bool; @d?; end; end",
            true,
        ),
        (
            "class variable",
            "class C; @@d?: int = 1; def self.g -> int; @@d?; end; end",
            true,
        ),
        ("constant", "X? = 1; X?", true),
        ("namespace constant", "module M; X? = 1; end; M::X?", true),
        ("class", "class R?; end; R?.new", true),
        ("module", "module R!; end; R!", true),
        ("enum", "enum S?; A; end; S?::A", true),
        ("enum member", "enum S; A?; end; S::A?", true),
        ("type alias", "type F! = bool; v: F! = true; v", true),
        ("type alias of a builtin name", "type int! = string", false),
        ("enum of a builtin name", "enum Time!; A; end", false),
        (
            "member of another's symbol",
            "enum S; Ready; READY?; end",
            false,
        ),
        ("constant of a prelude name", "Math! = 1; Math!", false),
    ] {
        let fixed = fix_all(source);
        if renamed {
            match Engine::new().type_check(&fixed) {
                Ok(checked) => {
                    assert!(
                        checked.diagnostics.is_empty(),
                        "{form}: {fixed}: {checked:?}"
                    )
                }
                Err(error) => panic!("{form}: {fixed}: {error}"),
            }
        } else {
            assert_eq!(fixed, source, "{form}");
            let Err(error) = Engine::new().type_check(source) else {
                panic!("{form} checks");
            };
            for diagnostic in error.diagnostics() {
                assert!(diagnostic.fixes.is_empty(), "{form}: {diagnostic:?}");
            }
        }
    }
}

#[test]
fn a_keyword_parameter_keeps_its_name() {
    // Calls name a keyword parameter with labels, strings that a rename of
    // the parameter would leave behind, and a host may call the function too.
    for source in [
        "def f(*, ok?: bool); end",
        "def f(*, k?: int = 1) -> int; k?; end; f(k?: 2)",
        "def f(a: int, k?:) -> int; k?; end; f(1, k?: 2)",
        "def f(*, k?: int = 1) -> int; k?; end; f",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
}

#[test]
fn a_fix_never_leaves_a_name_its_declaration_cannot_take() {
    for source in [
        // A type alias or an enum cannot take a builtin type's name.
        "type int! = string",
        "type Int! = string",
        "enum int!; A; end",
        "enum Time!; A; end",
        // A member cannot normalize to another's symbol.
        "enum S; Ready; READY?; end",
        // A constant cannot rebind a prelude namespace.
        "Math! = 1; Math!",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code, Code::NAME_SUFFIX, "{source}: {error}");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
    // A class may take a prelude namespace's name.
    let fixed = first_fix("class Math!; end; Math!.new");
    assert_eq!(fixed, "class Math; end; Math.new");
    Engine::new().type_check(&fixed).unwrap();
}

#[test]
fn type_alias_names_are_renamed_with_every_type_that_reads_them() {
    for (source, fixed) in [
        (
            "type Flag! = bool; value: Flag! = true; def f(x: Flag!) -> Flag!; x; end; f(value)",
            "type Flag = bool; value: Flag = true; def f(x: Flag) -> Flag; x; end; f(value)",
        ),
        (
            "module M; type F! = int; end; x: M::F! = 1; x",
            "module M; type F = int; end; x: M::F = 1; x",
        ),
        // A final `?` is the nullable marker, so `Flag!?` reads `Flag!`.
        (
            "type Flag! = bool; def f(x: Flag!?) -> int; 1; end; f(nil)",
            "type Flag = bool; def f(x: Flag?) -> int; 1; end; f(nil)",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        assert_eq!(migrate(source), fixed, "{source}");
        run(fixed);
    }
    // `Flag!?` may also mean an alias spelled so, and a symbol may name one.
    for source in [
        "type Flag!? = bool; def f(x: Flag!?) -> int; 1; end",
        "type Flag! = bool; p(:Flag!)",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        assert_eq!(error.diagnostics()[0].code, Code::NAME_SUFFIX, "{error}");
        assert!(error.diagnostics()[0].fixes.is_empty(), "{error:?}");
    }
}

#[test]
fn class_module_and_enum_names_are_renamed_wherever_they_are_used() {
    for (source, fixed) in [
        (
            "class Ready?; end; Ready?.new; 1",
            "class Ready; end; Ready.new; 1",
        ),
        (
            "class Ready?; end; x = Ready?; x.new; 1",
            "class Ready; end; x = Ready; x.new; 1",
        ),
        (
            "module Ready?; X = 1; def self.f -> int; X; end; end; [Ready?::X, Ready?.f]",
            "module Ready; X = 1; def self.f -> int; X; end; end; [Ready::X, Ready.f]",
        ),
        (
            "module A; module Ready?; X = 1; end; end; A::Ready?::X",
            "module A; module Ready; X = 1; end; end; A::Ready::X",
        ),
        (
            "module M; module Ready?; X = 1; end; def self.f -> int; Ready?::X; end; end; M.f",
            "module M; module Ready; X = 1; end; def self.f -> int; Ready::X; end; end; M.f",
        ),
        (
            "enum State?; A; end; State?::A == State?::A",
            "enum State; A; end; State::A == State::A",
        ),
        // A type names the class too; its final `?` is nullable.
        (
            "class Node!; end; def f(n: Node!, m: Node!?) -> array<Node!>; [n]; end; f(Node!.new, nil).length",
            "class Node; end; def f(n: Node, m: Node?) -> array<Node>; [n]; end; f(Node.new, nil).length",
        ),
        (
            "enum State!; A; end; def f(s: State!) -> State!; s; end; f(State!::A) == State!::A",
            "enum State; A; end; def f(s: State) -> State; s; end; f(State::A) == State::A",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
        assert_eq!(migrate(source), fixed, "{source}");
        run(fixed);
    }
    // A type reaches a nested class through its path.
    let source = "class A; class Node!; end; def g(n: Node!) -> A::Node!; n; end; end\n\
                  def f(n: A::Node!) -> A::Node!?; n; end";
    let fixed = "class A; class Node; end; def g(n: Node) -> A::Node; n; end; end\n\
                 def f(n: A::Node) -> A::Node?; n; end";
    assert_eq!(first_fix(source), fixed);
    Engine::new().type_check(fixed).unwrap();
    // A second declaration and a parent are renamed with the name, though
    // neither is valid: the fix leaves no reference to `Ready?` behind.
    for (source, fixed) in [
        (
            "class Ready?; end; class Ready?; end",
            "class Ready; end; class Ready; end",
        ),
        (
            "class Ready?; end; class B < Ready?; end",
            "class Ready; end; class B < Ready; end",
        ),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
    }
    // Where some use of the name cannot be told apart from another name, or
    // may lie past a syntax error, no fix is offered rather than a partial one.
    for source in [
        "class Ready?; end; p(:Ready?)",
        "class Ready?; end; p(Ready?(1))",
        "module A; module B!; X = 1; end; def self.B!(n: int) -> int; n; end; end\n\
         [A::B!::X, A::B!(1)]",
        "enum State; Done?; end; State::Done? == :done?",
        "class Ready?; end; Ready?.new; x = )",
        // `Node!?` is a nullable `Node!` to the type, though it may mean the class.
        "class Node!?; end; def f(n: Node!?); n; end; f(Node!?.new)",
        "class Ready!?; end; Ready!?.new; x = )",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        assert_eq!(
            error.diagnostics()[0].code,
            Code::NAME_SUFFIX,
            "{source}: {error}"
        );
        // Nor does a read the parser reports on its own keep a fix.
        for diagnostic in error.diagnostics() {
            if diagnostic.code == Code::NAME_SUFFIX {
                assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
            }
        }
    }
    let source = "class Ready!?; end; Ready!?.new; p(Ready!?)";
    let Err(error) = Engine::new().type_check(source) else {
        panic!("{source} checks");
    };
    let fixes: Vec<_> = error.diagnostics().iter().map(|d| d.fixes.len()).collect();
    assert_eq!(fixes, [1, 0, 0], "{error}");
    assert_eq!(first_fix(source), "class Ready; end; Ready.new; p(Ready)");
}

#[test]
fn a_fix_never_leaves_a_name_the_file_already_spells() {
    // Each rename would make two names one: `[x, x?]` would read one
    // binding twice, so V0003 is reported without a fix.
    for source in [
        "x = 1; x? = 2; [x, x?]",
        "def f(x: int, x?: int) -> int; x; end",
        "[1].each { |x| x? = x; p(x?) }",
        "def x -> int; 1; end; x? = 2; [x, x?]",
        "def x? -> int; 1; end; def x?? -> int; 2; end; [x?, x??]",
        "class Ready; end; class Ready?; end; [Ready, Ready?]",
        "module M; X = 1; X? = 2; end; M::X",
        "enum State; Ready; Ready?; end",
        "nil_ = 1; nil? = 2; [nil_, nil?]",
        "x? = 1; [x?, :x]",
        "x? = 1; [x?, %i[x]]",
        "x = 1; x? = 2; \"#{x}\"",
        "[1].each { |x!| x }",
        "def ok? -> bool; true; end; ok??",
        // A nullable type names `Node`.
        "class Node!; end; def f(a: Node?) -> int; 1; end",
    ] {
        let Err(error) = Engine::new().type_check(source) else {
            panic!("{source} checks");
        };
        let suffixes: Vec<_> = error
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::NAME_SUFFIX)
            .collect();
        assert!(!suffixes.is_empty(), "{source}: {error}");
        for diagnostic in suffixes {
            assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
        }
        assert_eq!(migrate(source), source);
    }
    // Of two fixes that each leave `x`, which applying both would merge,
    // only the first is offered; once it applies, `x` is spelled.
    let source = "x? = 1; x! = 2; [x?, x!]";
    let Err(error) = Engine::new().type_check(source) else {
        panic!("{source} checks");
    };
    let fixes: Vec<_> = error.diagnostics().iter().map(|d| d.fixes.len()).collect();
    assert_eq!(fixes, [1, 0], "{error}");
    assert_eq!(migrate(source), "x = 1; x! = 2; [x, x!]");
    // A name the file spells nowhere else is still left.
    for (source, fixed) in [
        ("x? = 1; [x?, :y, \"x\"]", "x = 1; [x, :y, \"x\"]"),
        ("nil? = 1; nil?", "nil_ = 1; nil_"),
        ("def x?? -> int; 2; end; x??", "def x? -> int; 2; end; x??"),
    ] {
        assert_eq!(first_fix(source), fixed, "{source}");
    }
}
