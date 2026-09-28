use super::super::test_support::Directory;
use super::*;
use crate::{CallOptions, CancellationToken, Limits};
use std::path::Path;

fn read(ctx: &mut CallContext, path: &Path, limit: usize) -> Result<Source> {
    ctx.checkpoint()?;
    let root = Root::new(path.parent().unwrap()).unwrap();
    let file = match root.open(ctx, Path::new(path.file_name().unwrap()))? {
        Opened::File(file) => file,
        Opened::Missing => {
            return Err(io_error(
                "opening module source",
                io::ErrorKind::NotFound.into(),
            ));
        }
        Opened::BrokenLink => return Err(escape()),
    };
    super::read(ctx, file, limit)
}

fn found(ctx: &mut CallContext, root: &Root, name: &str) -> bool {
    matches!(root.open(ctx, Path::new(name)).unwrap(), Opened::File(_))
}

#[test]
fn source_reads_preserve_bytes_stamps_and_exact_size_boundaries() {
    let directory = Directory::new();
    for length in [0, 1, 8191, 8192, 8193, 65537] {
        let mut bytes = vec![b'x'; length];
        if length > 2 {
            bytes[length - 2] = 0xff;
            bytes[length - 1] = 0;
        }
        let path = directory.write("source.vibe", &bytes);
        let metadata = fs::metadata(&path).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let source = read(&mut ctx, &path, length).unwrap();
        assert_eq!(source.contents.as_bytes().unwrap(), bytes);
        assert_eq!(source.stamp.size, length as u64);
        assert_eq!(source.stamp.modified, metadata.modified().unwrap());
        drop(source);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        if length != 0 {
            let error = read(&mut ctx, &path, length - 1).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Runtime);
            assert!(error.message.contains("source exceeds maximum size"));
            assert!(!ctx.exhausted());
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
    let path = directory.write("unbounded.vibe", b"x");
    let mut ctx = CallContext::new(CallOptions::default());
    assert_eq!(
        read(&mut ctx, &path, usize::MAX)
            .unwrap()
            .contents
            .as_bytes()
            .unwrap(),
        b"x"
    );
}

#[test]
fn missing_sources_are_distinguishable_from_other_io_failures() {
    let directory = Directory::new();
    let mut ctx = CallContext::new(CallOptions::default());
    let error = read(&mut ctx, &directory.0.join("absent.vibe"), 100).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
    assert!(!ctx.exhausted());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

struct Chunks<'a> {
    bytes: &'a [u8],
    position: usize,
    calls: usize,
    interrupt_first: bool,
    cancel_after: Option<(usize, CancellationToken)>,
}

impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.interrupt_first && self.calls == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let length = output.len().min(3).min(self.bytes.len() - self.position);
        output[..length].copy_from_slice(&self.bytes[self.position..self.position + length]);
        self.position += length;
        if let Some((at, cancellation)) = &self.cancel_after {
            if self.calls == *at {
                cancellation.cancel();
            }
        }
        Ok(length)
    }
}

#[test]
fn streaming_reads_handle_growth_interruptions_and_invalid_read_counts() {
    let mut stream = Chunks {
        bytes: b"12345",
        position: 0,
        calls: 0,
        interrupt_first: true,
        cancel_after: None,
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let error = read_contents(&mut ctx, &mut stream, 4).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert_eq!(stream.position, 5);
    assert_eq!(stream.calls, 3);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert!(!ctx.exhausted());

    struct Invalid;
    impl Read for Invalid {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            Ok(output.len() + 1)
        }
    }
    assert!(
        read_contents(&mut ctx, &mut Invalid, 10000)
            .unwrap_err()
            .message
            .contains("invalid module source read count")
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn reads_stop_at_work_memory_and_cancellation_boundaries() {
    let directory = Directory::new();
    let path = directory.write("source.vibe", &vec![b'x'; 65537]);
    for (limits, expected) in [
        (
            Limits {
                steps: Some(1024),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
        (
            Limits {
                steps: None,
                memory_bytes: Some(1024),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits,
            ..CallOptions::default()
        });
        assert_eq!(read(&mut ctx, &path, 65537).unwrap_err().kind, expected);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert!(ctx.exhausted());
    }
    let cancellation = CancellationToken::new();
    let mut stream = Chunks {
        bytes: b"123456789",
        position: 0,
        calls: 0,
        interrupt_first: false,
        cancel_after: Some((2, cancellation.clone())),
    };
    let mut ctx = CallContext::new(CallOptions {
        cancellation,
        ..CallOptions::default()
    });
    assert_eq!(
        read_contents(&mut ctx, &mut stream, 100).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert_eq!(stream.calls, 2);
    assert_eq!(stream.position, 6);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert_eq!(
        read_contents(&mut ctx, &mut stream, 100).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert_eq!(stream.calls, 2);
    assert_eq!(
        read(&mut ctx, &directory.0.join("absent"), 100)
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn rooted_access_rejects_absolute_and_parent_components() {
    let directory = Directory::new();
    directory.write("rules/nested/tool.vibe", b"data");
    let root = Root::new(&directory.0.join("rules")).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    assert!(found(&mut ctx, &root, "nested/tool.vibe"));
    assert!(!found(&mut ctx, &root, "missing/tool.vibe"));
    for name in [
        "../rules/nested/tool.vibe",
        "nested/../../outside",
        "/absolute",
    ] {
        let error = root.open(&mut ctx, Path::new(name)).unwrap_err();
        assert!(error.message.contains("escapes module root"), "{error}");
    }
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let mut ctx = CallContext::new(CallOptions {
        limits: Limits {
            memory_bytes: Some(1),
            steps: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    });
    assert_eq!(
        root.open(&mut ctx, Path::new("nested/tool.vibe"))
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn filename_checks_preserve_case_unicode_and_literal_components() {
    let directory = Directory::new();
    for name in ["Tool.vibe", "data/file.vibe", "é.vibe", "tool .vibe"] {
        directory.write(name, b"data");
    }
    let root = Root::new(&directory.0).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    for name in ["Tool.vibe", "data/file.vibe", "é.vibe", "tool .vibe"] {
        assert!(found(&mut ctx, &root, name), "{name}");
    }
    for name in [
        "tool.vibe",
        "Data/file.vibe",
        "É.vibe",
        "missing.vibe",
        "missing/file.vibe",
        "",
    ] {
        assert!(!found(&mut ctx, &root, name), "{name}");
    }
    for name in ["data/../Tool.vibe", "/Tool.vibe"] {
        assert!(root.open(&mut ctx, Path::new(name)).is_err(), "{name}");
    }
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut cancelled = CallContext::new(CallOptions {
        cancellation,
        ..CallOptions::default()
    });
    assert_eq!(
        root.open(&mut cancelled, Path::new("Tool.vibe"))
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn directory_fallback_charges_each_entry_and_releases_its_workspace() {
    let directory = Directory::new();
    for i in 0..32 {
        directory.write(&format!("entry-{i}.vibe"), b"");
    }
    let (parent, _) = platform::open_root(&directory.0).unwrap();
    let target = Path::new("entry-17.vibe");
    let mut ctx = CallContext::new(CallOptions::default());
    assert!(spelling_from_directory(&mut ctx, &parent, target.file_name().unwrap()).unwrap());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let mut ctx = CallContext::new(CallOptions {
        limits: Limits {
            steps: Some(4),
            ..Limits::default()
        },
        ..CallOptions::default()
    });
    let missing = directory.0.join("absent.vibe");
    assert_eq!(
        spelling_from_directory(&mut ctx, &parent, missing.file_name().unwrap())
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn symlink_checks_preserve_alias_names_and_reject_outside_missing_targets() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("root/actual/tool.vibe", b"data");
    directory.write("outside/secret.vibe", b"other");
    let path = directory.0.join("root");
    symlink("actual/tool.vibe", path.join("Alias.vibe")).unwrap();
    symlink(path.join("actual/tool.vibe"), path.join("Absolute.vibe")).unwrap();
    symlink("actual", path.join("link")).unwrap();
    symlink(directory.0.join("outside"), path.join("outside")).unwrap();
    symlink("../outside", path.join("relative-outside")).unwrap();
    symlink("absent.vibe", path.join("broken.vibe")).unwrap();
    let root = Root::new(&path).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    for name in ["Alias.vibe", "Absolute.vibe", "link/tool.vibe"] {
        let Opened::File(file) = root.open(&mut ctx, Path::new(name)).unwrap() else {
            panic!("{name}");
        };
        assert_eq!(
            super::read(&mut ctx, file, 100)
                .unwrap()
                .contents
                .as_bytes()
                .unwrap(),
            b"data"
        );
    }
    for name in [
        "outside/secret.vibe",
        "outside/absent/secret.vibe",
        "relative-outside/absent.vibe",
    ] {
        let error = root.open(&mut ctx, Path::new(name)).unwrap_err();
        assert!(error.message.contains("escapes module root"), "{error}");
    }
    assert!(matches!(
        root.open(&mut ctx, Path::new("broken.vibe")).unwrap(),
        Opened::BrokenLink
    ));
    assert!(matches!(
        root.open(&mut ctx, Path::new("link/absent.vibe")).unwrap(),
        Opened::Missing
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn non_regular_sources_use_nonblocking_close_on_exec_descriptors() {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let directory = Directory::new();
    let path = directory.0.join("fifo");
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is NUL-terminated and belongs to this test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (parent, _) = platform::open_root(&directory.0).unwrap();
    let file = platform::open(&parent, path.file_name().unwrap()).unwrap();
    // SAFETY: the descriptor is open, and these fcntl commands have no third argument.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: the same live descriptor and argument-free query are used here.
    let fd_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert!(fd_flags >= 0);
    assert_ne!(flags & libc::O_NONBLOCK, 0);
    assert_ne!(fd_flags & libc::FD_CLOEXEC, 0);
    drop(file);
    let mut ctx = CallContext::new(CallOptions::default());
    for path in [&path, &directory.0] {
        let error = read(&mut ctx, path, 100).unwrap_err();
        assert!(error.message.contains("not a regular file"), "{error}");
        assert!(!ctx.exhausted());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn native_filename_queries_do_not_depend_on_directory_enumeration() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let target = directory.write("private/Tool.vibe", b"");
    let parent = target.parent().unwrap();
    let handle = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority()).unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o111)).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let result = platform::stored_name(&mut ctx, &handle, target.file_name().unwrap());
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap(), Some(true));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn nofollow_operations_reject_replaced_file_and_directory_entries() {
    use cap_fs_ext::DirExt;
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let file = directory.write("outside/file.vibe", b"outside");
    fs::create_dir(directory.0.join("root")).unwrap();
    let parent =
        cap_std::fs::Dir::open_ambient_dir(directory.0.join("root"), cap_std::ambient_authority())
            .unwrap();
    symlink(file, directory.0.join("root/file.vibe")).unwrap();
    symlink(directory.0.join("outside"), directory.0.join("root/dir")).unwrap();
    assert!(platform::open(&parent, std::ffi::OsStr::new("file.vibe")).is_err());
    assert!(parent.open_dir_nofollow("dir").is_err());
}

#[cfg(unix)]
#[test]
fn incorrect_spelling_does_not_open_a_special_file() {
    let directory = Directory::new();
    let _listener =
        std::os::unix::net::UnixListener::bind(directory.0.join("Socket.vibe")).unwrap();
    let root = Root::new(&directory.0).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    assert!(matches!(
        root.open(&mut ctx, Path::new("socket.vibe")).unwrap(),
        Opened::Missing
    ));
    let error = root.open(&mut ctx, Path::new("Socket.vibe")).unwrap_err();
    assert!(error.message.contains("not a regular file"), "{error}");
    assert!(!ctx.exhausted());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn symlink_expansion_retains_parent_handles_and_enforces_directory_components() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("a/inner/marker", b"");
    directory.write("a/file.vibe", b"correct");
    directory.write("file.vibe", b"wrong");
    symlink("a/inner", directory.0.join("alias")).unwrap();
    symlink("alias/../file.vibe", directory.0.join("parent.vibe")).unwrap();
    symlink("file.vibe/../a/file.vibe", directory.0.join("invalid.vibe")).unwrap();
    symlink("file.vibe/.", directory.0.join("dot.vibe")).unwrap();
    symlink("file.vibe/", directory.0.join("slash.vibe")).unwrap();
    symlink("loop-b.vibe", directory.0.join("loop-a.vibe")).unwrap();
    symlink("loop-a.vibe", directory.0.join("loop-b.vibe")).unwrap();
    let root = Root::new(&directory.0).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let Opened::File(file) = root.open(&mut ctx, Path::new("parent.vibe")).unwrap() else {
        panic!();
    };
    assert_eq!(
        super::read(&mut ctx, file, 100)
            .unwrap()
            .contents
            .as_bytes()
            .unwrap(),
        b"correct"
    );
    for name in ["invalid.vibe", "dot.vibe", "slash.vibe"] {
        let error = root.open(&mut ctx, Path::new(name)).unwrap_err();
        assert!(error.message.contains("not a directory"), "{name}: {error}");
    }
    let error = root.open(&mut ctx, Path::new("loop-a.vibe")).unwrap_err();
    assert!(
        error.message.contains("too many module symlinks"),
        "{error}"
    );
    assert!(!ctx.exhausted());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn opened_sources_keep_bytes_and_metadata_when_the_path_is_replaced() {
    let directory = Directory::new();
    directory.write("tool.vibe", b"original");
    let root = Root::new(&directory.0).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let Opened::File(file) = root.open(&mut ctx, Path::new("tool.vibe")).unwrap() else {
        panic!();
    };
    let before = stamp(&mut ctx, &file).unwrap();
    fs::rename(directory.0.join("tool.vibe"), directory.0.join("old.vibe")).unwrap();
    directory.write("tool.vibe", b"replacement");
    let source = super::read(&mut ctx, file, 100).unwrap();
    assert_eq!(source.stamp, before);
    assert_eq!(source.contents.as_bytes().unwrap(), b"original");
    drop(source);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[cfg(unix)]
#[test]
fn concurrent_symlink_replacement_never_reads_outside_bytes() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    directory.write("root/inside.vibe", b"inside");
    directory.write("outside.vibe", b"outside");
    let path = directory.0.join("root");
    symlink("inside.vibe", path.join("choice.vibe")).unwrap();
    let root = Root::new(&path).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for i in 0..1000 {
                let target = if i % 2 == 0 {
                    "../outside.vibe"
                } else {
                    "inside.vibe"
                };
                symlink(target, path.join("next")).unwrap();
                fs::rename(path.join("next"), path.join("choice.vibe")).unwrap();
            }
        });
        for _ in 0..1000 {
            let mut ctx = CallContext::new(CallOptions::default());
            match root.open(&mut ctx, Path::new("choice.vibe")) {
                Ok(Opened::File(file)) => assert_eq!(
                    super::read(&mut ctx, file, 100)
                        .unwrap()
                        .contents
                        .as_bytes()
                        .unwrap(),
                    b"inside"
                ),
                Ok(Opened::Missing | Opened::BrokenLink) => {}
                Err(error) => assert!(
                    matches!(error.kind, ErrorKind::Runtime | ErrorKind::Name),
                    "{error}"
                ),
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    });
    assert!(found(
        &mut CallContext::new(CallOptions::default()),
        &root,
        "choice.vibe"
    ));
}
