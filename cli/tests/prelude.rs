//! `vibes prelude`: the builtin signature table as Vibescript declarations.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::vibes;

#[test]
fn prints_the_builtin_signature_table() {
    vibes(&["prelude"]).expect(0, &vibescript::signatures::prelude(), "");
    vibes(&["prelude", "--"]).expect(0, &vibescript::signatures::prelude(), "");
}

#[test]
fn refuses_arguments_and_prints_help() {
    vibes(&["prelude", "extra"]).fails("vibes prelude: does not accept positional arguments");
    let help = "NAME:\n   vibes prelude - print the builtin signatures as Vibescript declarations\n\n\
        USAGE:\n   vibes prelude [options]\n\n\
        OPTIONS:\n   --help, -h  show help\n";
    vibes(&["prelude", "--help"]).expect(0, help, "");
    vibes(&["help", "prelude"]).expect(0, help, "");
}
