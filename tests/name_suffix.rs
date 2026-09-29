use vibescript::{CallOptions, Engine, ErrorKind, Value, diagnostic::Code, stringify_json};

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
