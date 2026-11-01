use super::{ModuleConfig, test_support::Directory};
use crate::{CallOptions, Engine, ErrorKind, Value};

fn engine(directory: &Directory, deny: Vec<String>) -> Engine {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine
        .set_module_config(ModuleConfig {
            paths: vec![directory.0.clone()],
            allow: vec!["*".into()],
            deny,
            ..ModuleConfig::default()
        })
        .unwrap();
    engine
}

fn foreign_request(producer: &Engine, literal: &str) -> Value {
    // Rendering an imported instance runs its original code with the
    // receiving engine's module policy and require permission.
    producer
        .compile(&format!(
            "class Probe;def to_s -> string;require(\"{literal}\").value;end;end;Probe.new"
        ))
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

#[cfg(windows)]
#[test]
fn foreign_require_rejects_invalid_utf8_before_opening_a_denied_replacement_name() {
    let directory = Directory::new();
    directory.write("blocked�.vibe", b"def value -> string;\"secret\";end");
    directory.write("blocked���.vibe", b"def value -> string;\"secret\";end");
    let producer = engine(&directory, vec![]);
    let receiver = engine(&directory, vec!["blocked�".into(), "blocked���".into()])
        .compile(r##"def render(input: any) -> string;"#{input}";end"##)
        .unwrap();
    for literal in [
        r"blocked\xff",
        r"blocked\x80",
        r"blocked\xc3",
        r"blocked\xed\xa0\x80",
    ] {
        let input = foreign_request(&producer, literal);
        for _ in 0..2 {
            let error = receiver
                .call(
                    "render",
                    std::slice::from_ref(&input),
                    CallOptions {
                        allow_require: true,
                        ..CallOptions::default()
                    },
                )
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Argument, "{literal}: {error}");
            assert!(
                error.message.contains("module name must be UTF-8"),
                "{error}"
            );
        }
    }
}

#[test]
fn foreign_require_preserves_unicode_and_replacement_character_policy() {
    let directory = Directory::new();
    for name in ["café", "allowed�", "😀"] {
        directory.write(
            &format!("{name}.vibe"),
            b"def value -> string;\"loaded\";end",
        );
    }
    let producer = engine(&directory, vec![]);
    for name in ["café", "allowed�", "😀"] {
        let input = foreign_request(&producer, name);
        for denied in [false, true] {
            let receiver = engine(&directory, if denied { vec![name.into()] } else { vec![] })
                .compile(r##"def render(input: any) -> string;"#{input}";end"##)
                .unwrap();
            for _ in 0..2 {
                let result = receiver.call(
                    "render",
                    std::slice::from_ref(&input),
                    CallOptions {
                        allow_require: true,
                        ..CallOptions::default()
                    },
                );
                if denied {
                    assert!(result.unwrap_err().message.contains("denied by policy"));
                } else {
                    assert_eq!(result.unwrap().value.as_bytes(), Some(b"loaded".as_slice()));
                }
            }
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn foreign_require_keeps_unicode_symlinks_confined_to_the_module_root() {
    let directory = Directory::new();
    let outside = Directory::new();
    let source = b"def value -> string;\"loaded\";end";
    let path = directory.write("café.vibe", source);
    let target = outside.write("secret.vibe", source);
    let producer = engine(&directory, vec![]);
    let input = foreign_request(&producer, "café");
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, path).unwrap();
    let receiver = engine(&directory, vec![])
        .compile(r##"def render(input: any) -> string;"#{input}";end"##)
        .unwrap();
    let error = receiver
        .call(
            "render",
            &[input],
            CallOptions {
                allow_require: true,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(error.message.contains("escapes module root"), "{error}");
}
