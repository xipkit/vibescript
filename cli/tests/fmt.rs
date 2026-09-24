//! `vibes fmt`, ported from the Go reference's fmt_test.go and fmt_symlink_test.go.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use std::fs;
use support::{Files, vibes};

const UNFORMATTED: &str = "def run()  \n  1\t \nend";
const FORMATTED: &str = "def run()\n  1\nend\n";

#[test]
fn requires_a_path() {
    vibes(&["fmt"]).fails("vibes fmt: path required");
}

#[test]
fn prints_checks_and_writes_files() {
    let files = Files::new();
    let path = files.write("script.vibe", UNFORMATTED);
    vibes(&["fmt", &path]).expect(0, FORMATTED, "");
    vibes(&["fmt", "-check", &path]).fails("vibes fmt: 1 file(s) need formatting");
    assert_eq!(files.read("script.vibe"), UNFORMATTED);
    vibes(&["fmt", "-w", &path]).expect(0, "", "");
    assert_eq!(files.read("script.vibe"), FORMATTED);
    vibes(&["fmt", "-check", &path]).expect(0, "", "");
    // Both flags write and still report what needed formatting.
    let other = files.write("other.vibe", UNFORMATTED);
    vibes(&["fmt", "-w", "-check", &other]).fails("vibes fmt: 1 file(s) need formatting");
    assert_eq!(files.read("other.vibe"), FORMATTED);
}

#[test]
fn walks_directories_in_path_order() {
    let files = Files::new();
    files.write("b.vibe", "second  \n");
    files.write("a.vibe", "first\r\n");
    files.write("nested/c.vibe", "third\t\n\n\n");
    files.write("b-c.vibe", "dash\n");
    files.write("notes.txt", "ignored  \n");
    files.write("dir.vibe/d.vibe", "inner\n");
    let root = files.0.to_str().unwrap();
    vibes(&["fmt", root]).expect(0, "first\ndash\nsecond\ninner\nthird\n", "");
    vibes(&["fmt", "-check", root]).fails("vibes fmt: 3 file(s) need formatting");
    vibes(&["fmt", "-w", root]).expect(0, "", "");
    vibes(&["fmt", "-check", root]).expect(0, "", "");
    assert_eq!(files.read("notes.txt"), "ignored  \n");
}

#[test]
fn explicit_operands_are_filtered_and_deduplicated() {
    let files = Files::new();
    let script = files.write("a.vibe", "first  \n");
    let text = files.write("notes.txt", "ignored  \n");
    let root = files.0.to_str().unwrap();
    vibes(&["fmt", &text]).expect(0, "", "");
    vibes(&["fmt", root, &script, &script]).expect(0, "first\n", "");
    let missing = files.path("missing.vibe");
    vibes(&["fmt", &missing]).fails(&format!(
        "collect files: stat {missing}: stat {missing}: no such file or directory"
    ));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    #[test]
    fn recursive_discovery_never_follows_links() {
        for mode in ["stdout", "-check", "-w"] {
            let files = Files::new();
            let outside_dir = Files::new();
            let outside = outside_dir.write("outside.vibe", "outside  \r\n");
            let root = files.0.join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("a.vibe"), "inside  \r\n").unwrap();
            symlink(&outside, root.join("absolute.vibe")).unwrap();
            symlink("../../", root.join("relative-dir")).unwrap();
            symlink("absolute.vibe", root.join("chain.vibe")).unwrap();
            symlink("missing", root.join("dangling.vibe")).unwrap();
            symlink(&outside_dir.0, root.join("linked_directory")).unwrap();
            let root = root.to_str().unwrap();
            let run = if mode == "stdout" {
                vibes(&["fmt", root])
            } else {
                vibes(&["fmt", mode, root])
            };
            match mode {
                "stdout" => run.expect(0, "inside\n", ""),
                "-check" => run.fails("vibes fmt: 1 file(s) need formatting"),
                _ => run.expect(0, "", ""),
            }
            assert_eq!(outside_dir.read("outside.vibe"), "outside  \r\n", "{mode}");
            if mode == "-w" {
                assert_eq!(files.read("root/a.vibe"), "inside\n");
            }
        }
    }

    #[test]
    fn an_explicit_link_formats_its_target_and_keeps_the_link() {
        let files = Files::new();
        let target = files.write("target.txt", "explicit  \n");
        let alias = files.path("selected.vibe");
        symlink(&target, &alias).unwrap();
        vibes(&["fmt", "-w", &alias]).expect(0, "", "");
        assert_eq!(files.read("target.txt"), "explicit\n");
        assert!(
            fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn an_explicit_file_in_a_search_only_directory_is_formatted() {
        for mode in ["stdout", "-check", "-w"] {
            let files = Files::new();
            let parent = files.0.join("parent");
            fs::create_dir(&parent).unwrap();
            let path = parent.join("selected.vibe");
            fs::write(&path, "explicit  \n").unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o111)).unwrap();
            if fs::read_dir(&parent).is_ok() {
                // Directory permissions are not enforced, as for root.
                fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
                return;
            }
            let path = path.to_str().unwrap();
            let run = if mode == "stdout" {
                vibes(&["fmt", path])
            } else {
                vibes(&["fmt", mode, path])
            };
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
            match mode {
                "stdout" => run.expect(0, "explicit\n", ""),
                "-check" => run.fails("vibes fmt: 1 file(s) need formatting"),
                _ => run.expect(0, "", ""),
            }
            let want = if mode == "-w" {
                "explicit\n"
            } else {
                "explicit  \n"
            };
            assert_eq!(files.read("parent/selected.vibe"), want, "{mode}");
        }
    }

    #[test]
    fn rewriting_keeps_identity_permissions_and_hard_links() {
        let files = Files::new();
        let path = files.write("file.vibe", "body  \n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = files.path("hardlink");
        fs::hard_link(&path, &alias).unwrap();
        let before = fs::metadata(&path).unwrap();
        vibes(&["fmt", "-w", &path]).expect(0, "", "");
        let after = fs::metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(after.permissions().mode() & 0o777, 0o600);
        assert_eq!(files.read("hardlink"), "body\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        vibes(&["fmt", "-w", &path]).expect(0, "", "");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn non_regular_entries_are_skipped_or_rejected() {
        let files = Files::new();
        // Unix socket paths are short, so bind below the system temp directory.
        let dir = std::env::temp_dir().join(format!("vfmt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(dir.join("socket.vibe"));
        if let Ok(_listener) = listener {
            vibes(&["fmt", dir.to_str().unwrap()]).expect(0, "", "");
            let socket = dir.join("socket.vibe");
            let socket = socket.to_str().unwrap();
            vibes(&["fmt", socket])
                .fails(&format!("collect files: {socket} is not a regular file"));
        }
        let _ = fs::remove_dir_all(&dir);
        drop(files);
    }

    #[test]
    fn a_linked_directory_operand_is_walked_once() {
        let files = Files::new();
        files.write("root/a.vibe", "first\n");
        files.write("root/b.vibe", "second\n");
        let alias = files.path("selected");
        symlink(files.0.join("root"), &alias).unwrap();
        for target in [alias.clone(), format!("{alias}/.")] {
            vibes(&["fmt", &target, &format!("{target}/a.vibe")]).expect(0, "first\nsecond\n", "");
        }
    }

    #[test]
    fn many_directory_operands_stay_within_a_small_descriptor_limit() {
        let files = Files::new();
        let mut targets = Vec::new();
        let mut expected = String::new();
        for i in 0..128 {
            let name = format!("{i:03}/file.vibe");
            let source = format!("entry_{i:03}\n");
            files.write(&name, &source);
            expected.push_str(&source);
            targets.push(files.path(&format!("{i:03}")));
        }
        targets.reverse();
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("ulimit -n 64; exec \"$@\"")
            .arg("--")
            .arg(support::VIBES)
            .arg("fmt")
            .args(&targets)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}
