//! The language guide is shared by the crate API and the command line.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes, vibes_in};

#[test]
fn prints_the_bundled_language_guide() {
    vibes(&["guide"]).expect(0, vibescript::guide(), "");
    vibes(&["guide", "--"]).expect(0, vibescript::guide(), "");
}

#[test]
fn works_outside_the_checkout_and_ignores_local_guide_files() {
    let files = Files::new();
    files.write("guide", "puts \"local script\"\n");
    files.write("docs/language.md", "local language guide\n");
    vibes_in(Some(&files.0), &["guide"]).expect(0, vibescript::guide(), "");
}

#[test]
fn prints_command_help() {
    let help = "NAME:\n   vibes guide - print the language guide as Markdown\n\n\
        USAGE:\n   vibes guide [options]\n\n\
        OPTIONS:\n   --help, -h  show help\n";
    for args in [
        &["guide", "--help"][..],
        &["guide", "-h"],
        &["help", "guide"],
    ] {
        vibes(args).expect(0, help, "");
    }
}

#[test]
fn rejects_arguments_and_unknown_flags() {
    vibes(&["guide", "extra"]).fails("vibes guide: does not accept positional arguments");
    vibes(&["guide", "--", "extra"]).fails("vibes guide: does not accept positional arguments");
    vibes(&["guide", "--unknown"]).fails("flag provided but not defined: -unknown");
}
