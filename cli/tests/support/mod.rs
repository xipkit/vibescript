//! Helpers shared by the command integration tests.
#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

pub const VIBES: &str = env!("CARGO_BIN_EXE_vibes");
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The root help: the Go reference's, with `check` as the type check, plus
/// `guide`, `prelude` and `fix`.
pub const ROOT_HELP: &str = "NAME:
   vibes - run Vibescript programs and development tools

USAGE:
   vibes [global options] [command [command options]]

COMMANDS:
   run      execute a script file or inline snippet
   check    type check a script without executing it
   fmt      canonically format Vibescript source files
   analyze  analyze a script for lint issues
   test     discover and run Vibescript tests
   lsp      start the language server over stdio
   repl     start the interactive Vibescript REPL
   guide    print the language guide as Markdown
   prelude  print the builtin signatures as Vibescript declarations
   fix      apply the fixes of removed spellings and other compile diagnostics
   help, h  Shows a list of commands or help for one command

GLOBAL OPTIONS:
   --help, -h  show help
";

/// The Go reference's `vibes run` help without its `-check` flag.
pub const RUN_HELP: &str = "NAME:
   vibes run - execute a script file or inline snippet

USAGE:
   vibes run [options] <script> [args...]
   vibes run [options] -e SNIPPET

OPTIONS:
   --function string                              function to invoke; without it, top-level statements run when present, otherwise run
   -e string                                      evaluate an inline snippet instead of a script file
   --watch                                        re-run whenever the script or its modules change
   --module-path string [ --module-path string ]  add a module search directory (repeatable)
   --profile string                               execution quota profile: low, medium, high, xhigh (default: \"xhigh\")
   --step-quota int                               override the profile's step quota (-1 = unlimited)
   --memory-quota int                             override the profile's memory quota in bytes (-1 = unlimited)
   --recursion-limit int                          override the profile's recursion limit (-1 = unlimited, which can crash on infinite recursion)
   --help, -h                                     show help
";

/// A unique temporary directory, removed when the test finishes. Its path is
/// canonical, so Go-style reports print it unchanged.
pub struct Files(pub PathBuf);

impl Files {
    pub fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.cache/tmp");
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        loop {
            let path = base.join(format!(
                "cli-{}-{}-{}",
                env!("CARGO_CRATE_NAME"),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
    }

    /// Writes a file below the directory and returns its path.
    pub fn write(&self, name: &str, source: &str) -> String {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source).unwrap();
        path.to_str().unwrap().to_owned()
    }

    pub fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }

    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.0.join(name)).unwrap()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = remove_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

/// Removes a tree, restoring permissions tests may have taken away.
fn remove_all(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![path.to_owned()];
        while let Some(dir) = pending.pop() {
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        pending.push(entry.path());
                    }
                }
            }
        }
    }
    fs::remove_dir_all(path)
}

/// A finished process: status, stdout and stderr.
#[derive(Debug, PartialEq, Eq)]
pub struct Run {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    #[track_caller]
    pub fn expect(&self, status: i32, stdout: &str, stderr: &str) {
        assert_eq!(
            (self.status, self.stdout.as_str(), self.stderr.as_str()),
            (Some(status), stdout, stderr)
        );
    }

    /// Asserts a Go-style failure: status 1, no stdout and one error line.
    #[track_caller]
    pub fn fails(&self, message: &str) {
        self.expect(1, "", &format!("{message}\n"));
    }
}

pub fn vibes_in(dir: Option<&Path>, args: &[&str]) -> Run {
    let mut command = Command::new(VIBES);
    command.args(args).stdin(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.output().unwrap();
    Run {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

pub fn vibes(args: &[&str]) -> Run {
    vibes_in(None, args)
}
