mod common;

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, ModuleConfig, Value, stringify_json};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let path = base.join(format!(
                "checking-require-{}-{}",
                common::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => panic!("{error}"),
            }
        }
    }
    fn write(&self, name: &str, source: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![self.0.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        engine
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn witness(files: &Files, source: &str, options: CallOptions, expected: &str) {
    let script = files.engine().compile(source).unwrap();
    for _ in 0..2 {
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &[], options.clone()).unwrap();
        let json = stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(json.value.as_bytes(), Some(expected.as_bytes()), "{source}");
    }
}

fn foreign_witness(files: &Files, source: &str, expected: &str) {
    foreign_report(files, source, expected, false);
}

fn foreign_report(files: &Files, source: &str, expected: &str, diagnostics: bool) {
    let script = files.engine().compile(source).unwrap();
    let options = CallOptions::default();
    let result = script.call("run", &[], options.clone()).unwrap();
    let json = stringify_json(&result.value, options.clone()).unwrap();
    assert_eq!(json.value.as_bytes(), Some(expected.as_bytes()), "{source}");
    for report in [
        script.check_call("run", &[], &options).unwrap(),
        script.check_function("run", &options).unwrap(),
    ] {
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(
            !report.diagnostics.is_empty(),
            diagnostics,
            "{source}: {report:?}"
        );
    }
}

#[test]
fn foreign_classes_and_instances_keep_their_defining_source_and_heap() {
    let files = Files::new();
    files.write("models.vibe", "class Counter;property value:int;def initialize(n:int=3);@value=n;end;def add(n:int)->int;@value+=n;end;def self.kind->string;'counter';end;end;def counter_class;Counter;end;def counter;Counter.new;end");
    foreign_witness(
        &files,
        "def run;m=require(:models);c=m.counter_class();x=c.new(4);x.add(3);[x.value,x.class.kind(),c.kind()];end",
        "[7,\"counter\",\"counter\"]",
    );
    foreign_witness(
        &files,
        "def run;m=require(:models);x=m.counter();x.value=8;x.add(2);[x.value,m.counter().value];end",
        "[10,3]",
    );
}

#[test]
fn foreign_namespace_fields_and_nested_types_share_import_state() {
    let files = Files::new();
    files.write("names.vibe", "module Names;K=[2];module Inner;def self.value->int;7;end;end;def self.value->int;K[0];end;end;def namespace;Names;end");
    foreign_witness(
        &files,
        "def run;m=require(:names);n=m.namespace();n::K[0]+=3;[n.value(),n::Inner.value];end",
        "[5,7]",
    );
}

#[test]
fn foreign_instance_operators_rendering_and_introspection_use_source_methods() {
    let files = Files::new();
    files.write("operators.vibe", "class Number;def +(n:int)->int;5+n;end;def [](n:int)->int;10+n;end;def to_s->string;'number';end;end;def number;Number.new;end");
    foreign_witness(
        &files,
        "def run;n=require(:operators).number();[n+2,n[3],\"#{n}\",n.respond_to?(:to_s),n.class.respond_to?(:new)];end",
        "[7,13,\"number\",true,true]",
    );
}

#[test]
fn foreign_visibility_does_not_share_lexical_authority_with_equal_namespace_indexes() {
    let files = Files::new();
    files.write("private.vibe", "class C;protected;def secret;7;end;def +(n);7;end;def value=(n);7;end;public;def own(other);other.secret;end;end;def object;C.new;end");
    for expression in ["other.secret", "other+1", "begin;other.value=1;end"] {
        let source = format!(
            "class C;def use(other);{expression};end;end;def run;other=require(:private).object();begin;C.new.use(other);rescue RuntimeError;9;end;end"
        );
        foreign_report(&files, &source, "9", true);
    }
    foreign_witness(
        &files,
        "def run;m=require(:private);m.object().own(m.object());end",
        "7",
    );
}

#[test]
fn foreign_instances_keep_typed_mutations_and_pending_addresses() {
    let files = Files::new();
    files.write("boxes.vibe", "class Box;getter items:array<int>;def initialize;@items=[1,2];end;def append;@items.push(4);7;end;def work;@items[-1]+=append();@items;end;def bad;@items.push('bad');end;end;def box;Box.new;end");
    foreign_witness(
        &files,
        "def run;require(:boxes).box().work();end",
        "[1,9,4]",
    );
    foreign_report(
        &files,
        "def run;b=require(:boxes).box();begin;b.bad();rescue RuntimeError;nil;end;b.items;end",
        "[1,2]",
        true,
    );
}

#[test]
fn required_files_read_receiving_declarations_and_resolve_their_contracts() {
    let files = Files::new();
    files.write(
        "forward.vibe",
        "def make;Local.new(4);end;def take(x:Local)->int;x.value;end",
    );
    foreign_witness(
        &files,
        "class Local;property value:int;def initialize(@value);end;end;def run;m=require(:forward);m.take(m.make());end",
        "4",
    );
}

#[test]
fn foreign_nominal_types_do_not_collapse_equal_names_or_heap_indexes() {
    let files = Files::new();
    for (name, seed) in [("first.vibe", 3), ("second.vibe", 7)] {
        files.write(name, &format!("class C;property n:int;def initialize;@n={seed};end;end;def create;C.new;end;def take(x:C)->int;x.n;end"));
    }
    foreign_witness(
        &files,
        "def run;a=require(:first);b=require(:second);x=a.create();y=b.create();x.n=9;[x.n,y.n,x.class==y.class,a.take(x),b.take(y)];end",
        "[9,7,false,9,7]",
    );
    foreign_report(
        &files,
        "def run;a=require(:first);b=require(:second);begin;a.take(b.create());rescue;9;end;end",
        "9",
        true,
    );
}

#[test]
fn foreign_method_blocks_keep_lexical_receivers_and_control_transfers() {
    let files = Files::new();
    files.write(
        "iterator.vibe",
        "class Iterator;def each;yield(3);yield(99);end;end;def iterator;Iterator.new;end",
    );
    foreign_witness(
        &files,
        "class Receiver;getter n;def initialize;@n=0;end;def work(other);other.each{|n|@n+=n;break 7};@n;end;end;def run;Receiver.new.work(require(:iterator).iterator());end",
        "3",
    );
    foreign_witness(
        &files,
        "def run;require(:iterator).iterator().each{|n|return n+2};99;end",
        "5",
    );
}

#[test]
fn whole_file_analysis_keeps_foreign_constructor_and_method_owners() {
    let files = Files::new();
    files.write("whole.vibe", "class C;property n:int;def initialize(n:int=3);@n=n;end;def value->int;@n;end;end;def klass;C;end");
    let script = files
        .engine()
        .compile("m=require(:whole);c=m.klass().new(5);c.n=7;c.value")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap().value.as_int(),
        Some(7)
    );
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn required_host_contracts_use_defining_types_then_receiving_declarations_without_effects() {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature, SignatureParam};
    let files = Files::new();
    files.write("host.vibe", "def value(x);observe(x);end");
    files.write(
        "owned.vibe",
        "class Local;end;def value(x);observe(x);end;def local;Local.new;end",
    );
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let mut engine = files.engine();
    engine.register_method(
        "observe",
        HostMethod::new("observe", move |_, _, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Value::int(7))
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "item".into(),
                ty: "Local".into(),
                optional: false,
            }],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    for (body, rejected, expected) in [
        ("require(:host).value(Local.new)", false, 7),
        ("m=require(:owned);m.value(m.local())", false, 7),
        (
            "begin;require(:owned).value(Local.new);rescue;9;end",
            true,
            9,
        ),
    ] {
        let script = engine
            .compile(&format!("class Local;end;def run->int;{body};end"))
            .unwrap();
        let before = count.load(Ordering::Relaxed);
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{body}: {report:?}");
        assert_eq!(
            !report.diagnostics.is_empty(),
            rejected,
            "{body}: {report:?}"
        );
        assert_eq!(count.load(Ordering::Relaxed), before);
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(output.value.as_int(), Some(expected), "{body}");
        assert_eq!(
            count.load(Ordering::Relaxed),
            before + usize::from(!rejected)
        );
    }
}

#[test]
fn imported_functions_use_private_state_defaults_and_published_names() {
    let files = Files::new();
    files.write(
        "numbers.vibe",
        "base=10;def value(n:int=2)->int;base+n;end;def increment;base+=1;end",
    );
    for source in [
        "def run; m=require(:numbers); [m.value(),m.value(4)];end",
        "def run; require(:numbers,as: :N); [N.value(),value(4)];end",
        "def run; m=require(:numbers); m.increment(); n=require('numbers.vibe'); [m.value(),n.value(4)];end",
        "def run; require(:numbers,as: :N); require(:numbers,as: :N); [N.value(),value(4)];end",
    ] {
        let expected = if source.contains("m.increment") {
            "[13,15]"
        } else {
            "[12,14]"
        };
        witness(&files, source, CallOptions::default(), expected);
    }
}

#[test]
fn relative_imports_keep_defining_origins_and_receiving_roots() {
    let files = Files::new();
    files.write(
        "a/root.vibe",
        "m=require('./child');def value; m.value()+root();end",
    );
    files.write("a/child.vibe", "def value;7;end");
    files.write("child.vibe", "def value;99;end");
    witness(
        &files,
        "def root;3;end;def run;require('a/root').value();end",
        CallOptions::default(),
        "10",
    );
}

#[test]
fn failed_initialization_can_retry_without_reusing_private_bindings() {
    let files = Files::new();
    files.write(
        "retry.vibe",
        "state[0]+=1;n=state[0];if n==1;raise 'retry';end;def number;n;end",
    );
    let mut options = CallOptions::default();
    options
        .globals
        .insert("state".into(), Value::array(vec![Value::int(0)]));
    witness(
        &files,
        "def run;begin;require(:retry);rescue;nil;end;m=require(:retry);[m.number(),state[0]];end",
        options,
        "[2,2]",
    );
}

#[test]
fn circular_imports_and_foreign_diagnostics_keep_their_sources() {
    let files = Files::new();
    files.write("a.vibe", "require(:b);def value;1;end");
    files.write("b.vibe", "require(:a);def value;2;end");
    files.write("bad.vibe", "def wrong->int;false;end");
    for (source, fragment, filename) in [
        ("def run;require(:a);end", "circular", "b.vibe"),
        (
            "def run;require(:bad).wrong();end",
            "Return value",
            "bad.vibe",
        ),
    ] {
        let script = files.engine().compile(source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        let diagnostic = report
            .diagnostics
            .iter()
            .find(|d| d.message.contains(fragment))
            .unwrap_or_else(|| panic!("{report:?}"));
        assert!(
            diagnostic
                .filename
                .as_ref()
                .is_some_and(|f| f.ends_with(filename.as_bytes())),
            "{diagnostic:?}"
        );
        assert!(matches!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Runtime | ErrorKind::Type
        ));
    }
}

#[test]
fn analysis_preserves_host_and_writer_effects_for_execution_only() {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature};
    let files = Files::new();
    files.write("effects.vibe", "tick();puts('loaded');def value;7;end");
    let mut engine = files.engine();
    let count = Arc::new(AtomicUsize::new(0));
    let called = count.clone();
    engine.register_method(
        "tick",
        HostMethod::new("tick", move |_, _, _| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        })
        .with_signature(Signature {
            params: vec![],
            result: "nil".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    let called = count.clone();
    engine.set_output_writer(move |_, _| {
        called.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let script = engine
        .compile("def run;require(:effects).value();end")
        .unwrap();
    for report in [
        script
            .check_call("run", &[], &CallOptions::default())
            .unwrap(),
        script
            .check_function("run", &CallOptions::default())
            .unwrap(),
        script.check(&CallOptions::default()).unwrap(),
    ] {
        assert!(report.is_clean(), "{report:?}");
    }
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

#[test]
fn conditional_publication_retains_absent_and_present_paths() {
    let files = Files::new();
    files.write("numbers.vibe", "def value->int;7;end");
    let script = files.engine().compile("def run(flag:bool)->int;if flag;require(:numbers,as: :N);end;if flag;N.value();else;1;end;end").unwrap();
    for (flag, expected) in [(true, 7), (false, 1)] {
        let args = [Value::boolean(flag)];
        let report = script
            .check_call("run", &args, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            script
                .call("run", &args, CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(expected)
        );
    }
    let script = files
        .engine()
        .compile("def run(flag:bool)->int;if flag;require(:numbers,as: :N);end;N.value();end")
        .unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.is_clean(), "{report:?}");
    assert!(
        script
            .call("run", &[Value::boolean(false)], CallOptions::default())
            .is_err()
    );
}

#[test]
fn invalid_requests_are_known_errors_and_dynamic_requests_remain_explicit() {
    let files = Files::new();
    files.write("valid.vibe", "def value;7;end");
    files.write("syntax.vibe", "def !");
    for expression in [
        "require()",
        "require(1)",
        "require(:valid, nope: 1)",
        "require(:valid, as: 1)",
        "require(:valid, as: 'def')",
        "require(:valid) { 1 }",
        "require(:missing)",
        "require(:syntax)",
        "require('../escape')",
    ] {
        let script = files
            .engine()
            .compile(&format!("def run;{expression};end"))
            .unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{expression}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{expression}: {report:?}");
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{expression}"
        );
    }
    let script = files
        .engine()
        .compile("def run(name:string);require(name);end")
        .unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.incomplete.is_empty(), "{report:?}");
}

#[test]
fn aliases_reject_existing_locals_roots_and_builtin_names() {
    let files = Files::new();
    files.write("valid.vibe", "def value;7;end");
    for source in [
        "def run;x=1;require(:valid,as: :x);end",
        "def run;require(:valid,as: :JSON);end",
        "def existing;7;end;def run;require(:valid,as: :existing);end",
        "def existing;7;end;def run;existing=require(:valid);require(:valid,as: :existing);end",
    ] {
        let script = files.engine().compile(source).unwrap();
        assert!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .message
                .contains("alias already defined")
        );
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("alias already defined")),
            "{report:?}"
        );
        assert!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .message
                .contains("alias already defined")
        );
    }
    let script = files
        .engine()
        .compile("def run;require(:valid,as: :external);end")
        .unwrap();
    let mut options = CallOptions::default();
    options.globals.insert("external".into(), Value::int(1));
    let report = script.check_call("run", &[], &options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(script.call("run", &[], options).is_err());
}

#[test]
fn whole_file_checks_unused_imported_declarations_but_exact_calls_keep_their_scope() {
    let files = Files::new();
    files.write(
        "declarations.vibe",
        "def value->int;7;end;def wrong->int;false;end",
    );
    let script = files
        .engine()
        .compile("def run;require(:declarations).value();end")
        .unwrap();
    let exact = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(exact.is_clean(), "{exact:?}");
    let whole = script.check(&CallOptions::default()).unwrap();
    assert!(whole.incomplete.is_empty(), "{whole:?}");
    assert!(
        whole.diagnostics.iter().any(|d| d.function == "wrong"),
        "{whole:?}"
    );

    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn imported_enums_and_initializer_mutations_reach_receiving_contracts_and_addresses() {
    let files = Files::new();
    files.write(
        "colors.vibe",
        "enum Color;Red;Blue;end;def pick(c:Color)->Color;c;end",
    );
    witness(
        &files,
        "def take(c:Color)->Color;c;end;def run;require(:colors);take(:red)==Color::Red;end",
        CallOptions::default(),
        "true",
    );
    files.write("grow.vibe", "items.push(2);def one;7;end");
    let mut options = CallOptions::default();
    options
        .globals
        .insert("items".into(), Value::array(vec![Value::int(1)]));
    witness(
        &files,
        "def run;items[-1]+=require(:grow).one();items;end",
        options,
        "[8,2]",
    );
}

#[test]
fn import_policy_strict_effects_cancellation_and_limits_precede_initialization() {
    use std::time::Instant;
    use vibescript::{CancellationToken, Limits};
    let files = Files::new();
    files.write("valid.vibe", "def value;7;end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    let script = engine
        .compile("def run;require(:valid).value();end")
        .unwrap();
    let denied = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(denied.incomplete.is_empty(), "{denied:?}");
    assert!(
        denied
            .diagnostics
            .iter()
            .any(|d| d.message.contains("strict effects")),
        "{denied:?}"
    );
    let options = CallOptions {
        allow_require: true,
        ..CallOptions::default()
    };
    assert!(script.check_call("run", &[], &options).unwrap().is_clean());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for (options, kind) in [
        (
            CallOptions {
                cancellation: cancelled,
                ..options.clone()
            },
            ErrorKind::Cancelled,
        ),
        (
            CallOptions {
                deadline: Some(Instant::now()),
                ..options.clone()
            },
            ErrorKind::Deadline,
        ),
        (
            CallOptions {
                limits: Limits {
                    steps: Some(100),
                    ..Limits::default()
                },
                ..options.clone()
            },
            ErrorKind::Steps,
        ),
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(1024),
                    ..Limits::default()
                },
                ..options
            },
            ErrorKind::Memory,
        ),
    ] {
        assert_eq!(
            script.check_call("run", &[], &options).unwrap_err().kind,
            kind
        );
    }
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            deny: vec!["valid".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("def run;require(:valid);end").unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(script.call("run", &[], CallOptions::default()).is_err());
}

#[test]
fn required_namespace_initializers_precede_file_statements_and_early_returns() {
    let files = Files::new();
    files.write(
        "order.vibe",
        "events.push('file');module M;events.push('namespace');end;def value;events;end",
    );
    let options = CallOptions {
        globals: [("events".into(), Value::array(vec![]))].into(),
        ..CallOptions::default()
    };
    witness(
        &files,
        "def run;require(:order).value();end",
        options,
        "[\"namespace\",\"file\"]",
    );
    files.write(
        "invalid.vibe",
        "return;module M;K=1-'bad';end;def value;7;end",
    );
    let script = files
        .engine()
        .compile("def run;require(:invalid).value();end")
        .unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Operator")),
        "{report:?}"
    );
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
}

#[test]
fn imported_callbacks_preserve_lexical_mutations_breaks_and_returns() {
    let files = Files::new();
    files.write("blocks.vibe", "def sequence;yield(1);yield(2);end");
    witness(
        &files,
        "def run;n=10;v=require(:blocks).sequence{|x|n+=x;if x==2;break 7;end;0};[n,v];end",
        CallOptions::default(),
        "[13,7]",
    );
    witness(
        &files,
        "def run->int;require(:blocks).sequence{|x|return x+7};0;end",
        CallOptions::default(),
        "8",
    );
}

#[test]
fn literal_module_alternatives_keep_export_calls_and_attachment_rules() {
    let files = Files::new();
    files.write("first.vibe", "def value(n:int)->int;n+10;end");
    files.write("second.vibe", "def value(n:int)->int;n+20;end");
    let script = files
        .engine()
        .compile("def run(flag:bool)->int;m=require(flag ? :first : :second);m[:value](1);end")
        .unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    for (flag, expected) in [(true, 11), (false, 21)] {
        assert_eq!(
            script
                .call("run", &[Value::boolean(flag)], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(expected)
        );
    }
    let script = files
        .engine()
        .compile("def run;m=require(:first);f=m[:value];f(1);end")
        .unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(script.call("run", &[], CallOptions::default()).is_err());
}

#[test]
fn conditional_failed_imports_do_not_seed_a_fresh_path_with_private_state() {
    let files = Files::new();
    files.write(
        "conditional.vibe",
        "n||=0;n+=1;if controls[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    );
    let script = files.engine().compile("def run(flag:bool)->int;if flag;controls[0]=true;begin;require(:conditional);rescue;nil;end;end;controls[0]=false;require(:conditional).value();end").unwrap();
    let options = CallOptions {
        globals: [("controls".into(), Value::array(vec![Value::boolean(false)]))].into(),
        ..CallOptions::default()
    };
    for flag in [false, true] {
        let args = [Value::boolean(flag)];
        assert_eq!(
            script
                .call("run", &args, options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        let report = script.check_call("run", &args, &options).unwrap();
        assert!(report.is_clean(), "{report:?}");
    }
    let report = script.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn import_lifetimes_remain_correlated_through_namespace_initializers() {
    let files = Files::new();
    files.write(
        "conditional.vibe",
        "n||=0;n+=1;if controls[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    );
    files.write("outer.vibe", "module M;if controls[0];begin;require(:conditional);rescue;nil;end;end;end;controls[0]=false;m=require(:conditional);def value->int;m.value();end");
    let script = files
        .engine()
        .compile("def run(flag:bool)->int;controls[0]=flag;require(:outer).value();end")
        .unwrap();
    let options = CallOptions {
        globals: [("controls".into(), Value::array(vec![Value::boolean(false)]))].into(),
        ..CallOptions::default()
    };
    for flag in [false, true] {
        let args = [Value::boolean(flag)];
        assert_eq!(
            script
                .call("run", &args, options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        assert!(
            script
                .check_call("run", &args, &options)
                .unwrap()
                .is_clean()
        );
    }
    let report = script.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn import_lifetimes_remain_correlated_through_collection_callbacks() {
    let files = Files::new();
    files.write(
        "conditional.vibe",
        "n||=0;n+=1;if controls[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    );
    let options = CallOptions {
        globals: [("controls".into(), Value::array(vec![Value::boolean(false)]))].into(),
        ..CallOptions::default()
    };
    let attempt = "if flag;controls[0]=true;begin;require(:conditional);rescue;nil;end;end";
    for call in [
        "[0].each{|_|BODY}",
        "[0].map{|_|BODY}",
        "[0].select{|_|BODY;true}",
        "[0].sort_by{|_|BODY;0}",
        "[0,1].sort{|a,b|BODY;0}",
        "1.times{BODY}",
        "[0].each_slice(1){|_|BODY}",
        "[0].each_cons(1){|_|BODY}",
        "[0].cycle(1){|_|BODY}",
        "a=[0];a.delete_if{|_|BODY;false}",
        "a=[0];a.fill{|_|BODY;0}",
        "{}.fetch(:missing){|_|BODY;0}",
        "{}.fetch_values(:missing){|_|BODY;0}",
        "'x'.each_char{|_|BODY}",
        "'x'.gsub('x'){|_|BODY;'y'}",
        "{a:0}.merge({a:1}){|k,a,b|BODY;0}",
        "{a:0}.deep_transform_keys{|k|BODY;k}",
        "[0].reduce(0){|sum,_|BODY;sum}",
        "loop{BODY;break}",
        "0.tap{|_|BODY}",
    ] {
        let call = call.replace("BODY", attempt);
        let source = format!(
            "def run(flag:bool)->int;{call};controls[0]=false;require(:conditional).value();end"
        );
        let script = files.engine().compile(&source).unwrap();
        for flag in [false, true] {
            let args = [Value::boolean(flag)];
            assert_eq!(
                script
                    .call("run", &args, options.clone())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
            assert!(
                script
                    .check_call("run", &args, &options)
                    .unwrap()
                    .is_clean()
            );
        }
        let report = script.check_function("run", &options).unwrap();
        assert!(report.is_clean(), "{call}: {report:?}");
    }
}

#[test]
fn import_lifetimes_remain_correlated_through_named_reductions() {
    let files = Files::new();
    files.write(
        "conditional.vibe",
        "n||=0;n+=1;if controls[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    );
    files.write(
        "selector.vibe",
        "def select(n);if controls[0];begin;require(:conditional);rescue;nil;end;end;0;end",
    );
    let script = files.engine().compile("def run(flag:bool)->int;controls[0]=flag;[0].reduce(require(:selector),:select);controls[0]=false;require(:conditional).value();end").unwrap();
    let options = CallOptions {
        globals: [("controls".into(), Value::array(vec![Value::boolean(false)]))].into(),
        ..CallOptions::default()
    };
    for flag in [false, true] {
        let args = [Value::boolean(flag)];
        assert_eq!(
            script
                .call("run", &args, options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        let report = script.check_call("run", &args, &options).unwrap();
        assert!(report.is_clean(), "{report:?}");
    }
    let report = script.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn repeated_abstract_callbacks_keep_first_import_and_cached_paths_separate() {
    let files = Files::new();
    files.write("numbers.vibe", "n=7;def value->int;n;end");
    let script = files.engine().compile("def run(items:array<int>,flag:bool)->int;items.each{|_|if flag;require(:numbers);end};require(:numbers).value();end").unwrap();
    let options = CallOptions::default();
    for count in [0, 1, 3] {
        for flag in [false, true] {
            let args = [
                Value::array(vec![Value::int(0); count]),
                Value::boolean(flag),
            ];
            assert_eq!(
                script
                    .call("run", &args, options.clone())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
        }
    }
    let report = script.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn optional_value_reads_preserve_imports_from_function_fallbacks() {
    let files = Files::new();
    files.write(
        "conditional.vibe",
        "n||=0;n+=1;if controls[0];raise 'retry';end;def value->int;if n==1;7;else;false;end;end",
    );
    let script = files.engine().compile("def fallback;events[0]+=1;controls[0]=true;begin;require(:conditional);rescue;nil;end;end;def once;yield;end;def run(flag:bool);ignored=if flag;begin;fallback=7;end;end;[0].length;once{fallback};controls[0]=false;[require(:conditional).value(),events[0]];end").unwrap();
    let options = CallOptions {
        globals: [
            ("controls".into(), Value::array(vec![Value::boolean(false)])),
            ("events".into(), Value::array(vec![Value::int(0)])),
        ]
        .into(),
        ..CallOptions::default()
    };
    for (flag, expected) in [(false, "[7,1]"), (true, "[7,0]")] {
        let args = [Value::boolean(flag)];
        let result = script.call("run", &args, options.clone()).unwrap();
        let json = stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(json.value.as_bytes(), Some(expected.as_bytes()));
        let report = script.check_call("run", &args, &options).unwrap();
        assert!(report.is_clean(), "{report:?}");
    }
    let report = script.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn whole_file_declarations_preserve_imported_private_state_alternatives() {
    use vibescript::{HostMethod, Signature};
    let files = Files::new();
    files.write("first.vibe", "n=1;def value->int;n;end");
    files.write("second.vibe", "n=2;def value->int;n;end");
    for flag in [false, true] {
        let mut engine = files.engine();
        engine.register_method(
            "choose",
            HostMethod::new("choose", move |_, _, _| Ok(Value::boolean(flag)))
                .with_signature(Signature {
                    params: vec![],
                    result: "bool".into(),
                    accepts_block: false,
                })
                .unwrap(),
        );
        let script = engine.compile("if choose();require(:first,as: :M);else;require(:second,as: :M);end;def run->int;M.value();end;run()").unwrap();
        let options = CallOptions::default();
        assert_eq!(
            script
                .call("__main__", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(if flag { 1 } else { 2 })
        );
        let report = script.check(&options).unwrap();
        assert!(report.is_clean(), "{report:?}");
    }
}
