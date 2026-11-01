use super::{ModuleConfig, test_support::Directory};
use crate::{CallOptions, Engine, Value};

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

#[test]
fn public_compilation_rejects_invalid_utf8_module_literals() {
    let directory = Directory::new();
    directory.write("blocked�.vibe", b"def value -> string;\"secret\";end");
    directory.write("blocked���.vibe", b"def value -> string;\"secret\";end");
    let producer = engine(&directory, vec![]);
    for literal in [
        r"blocked\xff",
        r"blocked\x80",
        r"blocked\xc3",
        r"blocked\xed\xa0\x80",
    ] {
        let error = producer
            .compile(&format!(
                "class Probe;def to_s -> string;require(\"{literal}\").value;end;end;Probe.new"
            ))
            .err()
            .unwrap();
        assert!(error.message.contains("V0309"), "{literal}: {error}");
    }
}

fn read_bytes(
    resolver: &super::resolver::Resolver,
    input: &[u8],
) -> crate::Result<Option<super::files::Source>> {
    let mut context = crate::CallContext::new(CallOptions::default());
    let mut candidates = resolver.candidates(&mut context, input, None)?;
    let candidate = candidates.next(&mut context)?.unwrap();
    resolver.read(&mut context, &candidate)
}

#[cfg(windows)]
#[test]
fn invalid_module_bytes_cannot_open_a_denied_replacement_name() {
    let directory = Directory::new();
    directory.write("blocked�.vibe", b"secret");
    directory.write("blocked���.vibe", b"secret");
    let resolver = super::resolver::Resolver::new(
        &[directory.0.clone()],
        &["*".into()],
        &["blocked�".into(), "blocked���".into()],
        100,
    )
    .unwrap();
    for input in [
        b"blocked\xff".as_slice(),
        b"blocked\x80",
        b"blocked\xc3",
        b"blocked\xed\xa0\x80",
    ] {
        let error = read_bytes(&resolver, input).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Argument);
        assert!(
            error.message.contains("module name must be UTF-8"),
            "{error}"
        );
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
    assert_eq!(error.kind, crate::ErrorKind::Runtime);
    assert!(error.message.contains("escapes module root"), "{error}");
}

#[cfg(any(unix, target_os = "wasi"))]
#[test]
fn resolver_preserves_native_filename_bytes() {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "wasi")]
    use std::os::wasi::ffi::OsStrExt;

    let directory = Directory::new();
    directory.write("raw�.vibe", b"replacement");
    std::fs::write(
        directory
            .0
            .join(std::ffi::OsStr::from_bytes(b"raw\xff.vibe")),
        b"raw",
    )
    .unwrap();
    let resolver = super::resolver::Resolver::new(
        &[directory.0.clone()],
        &["*".into()],
        &["raw�".into()],
        100,
    )
    .unwrap();
    let source = read_bytes(&resolver, b"raw\xff").unwrap().unwrap();
    assert_eq!(source.contents.as_bytes(), Some(b"raw".as_slice()));
}
