//! Integration suites share a binary to avoid repeatedly linking the interpreter.
//! Register new root test files below; the layout test checks none are omitted.

// Keep each suite's relative helper modules without rewriting concurrent edits.
#![allow(clippy::duplicate_mod)]

#[path = "address.rs"]
mod address;
#[path = "array_chunk_block.rs"]
mod array_chunk_block;
#[path = "array_combinatorics.rs"]
mod array_combinatorics;
#[path = "array_index_offsets.rs"]
mod array_index_offsets;
#[path = "array_sets.rs"]
mod array_sets;
#[path = "async_capability_receivers.rs"]
mod async_capability_receivers;
#[path = "asynchronous.rs"]
mod asynchronous;
#[path = "bare_calls.rs"]
mod bare_calls;
#[path = "bindings.rs"]
mod bindings;
#[path = "blocks.rs"]
mod blocks;
#[path = "builtin_signatures.rs"]
mod builtin_signatures;
#[path = "builtins.rs"]
mod builtins;
#[path = "call_member.rs"]
mod call_member;
#[path = "calls.rs"]
mod calls;
#[path = "capabilities.rs"]
mod capabilities;
#[path = "capability_publication.rs"]
mod capability_publication;
#[path = "capability_receivers.rs"]
mod capability_receivers;
#[path = "capability_snapshots.rs"]
mod capability_snapshots;
#[path = "checker_diff.rs"]
mod checker_diff;
#[path = "checker_gaps.rs"]
mod checker_gaps;
#[path = "classes.rs"]
mod classes;
#[path = "collection_block_signatures.rs"]
mod collection_block_signatures;
#[path = "collection_counts.rs"]
mod collection_counts;
#[path = "collection_messages.rs"]
mod collection_messages;
#[path = "commands.rs"]
mod commands;
#[path = "computed_calls.rs"]
mod computed_calls;
#[path = "control.rs"]
mod control;
#[path = "core.rs"]
mod core;
#[path = "declarations.rs"]
mod declarations;
#[path = "diagnostics.rs"]
mod diagnostics;
#[path = "docs.rs"]
mod docs;
#[path = "duration.rs"]
mod duration;
#[path = "enums.rs"]
mod enums;
#[path = "equality.rs"]
mod equality;
#[path = "error_classes.rs"]
mod error_classes;
#[path = "error_handling.rs"]
mod error_handling;
#[path = "floor_division.rs"]
mod floor_division;
#[path = "foreign_programs.rs"]
mod foreign_programs;
#[path = "formatting.rs"]
mod formatting;
#[path = "forwarding.rs"]
mod forwarding;
#[path = "globals.rs"]
mod globals;
#[path = "hash_blocks.rs"]
mod hash_blocks;
#[path = "hash_new.rs"]
mod hash_new;
#[path = "hash_shorthand.rs"]
mod hash_shorthand;
#[path = "hashes.rs"]
mod hashes;
#[path = "host_blocks.rs"]
mod host_blocks;
#[path = "host_boundary_snapshots.rs"]
mod host_boundary_snapshots;
#[path = "host_declarations.rs"]
mod host_declarations;
#[path = "host_globals.rs"]
mod host_globals;
#[path = "host_signatures.rs"]
mod host_signatures;
#[path = "integers.rs"]
mod integers;
#[path = "introspection.rs"]
mod introspection;
#[path = "iteration.rs"]
mod iteration;
#[path = "json.rs"]
mod json;
#[path = "json_depth_codec.rs"]
mod json_depth_codec;
#[path = "json_depth_collections.rs"]
mod json_depth_collections;
#[path = "json_depth_rendering.rs"]
mod json_depth_rendering;
#[path = "json_depth_values.rs"]
mod json_depth_values;
#[path = "kernel_loop.rs"]
mod kernel_loop;
#[path = "keyword_parameters.rs"]
mod keyword_parameters;
#[path = "language.rs"]
mod language;
#[path = "limits.rs"]
mod limits;
#[path = "literals.rs"]
mod literals;
#[path = "members.rs"]
mod members;
#[path = "modules.rs"]
mod modules;
#[path = "money.rs"]
mod money;
#[path = "mutable_blocks.rs"]
mod mutable_blocks;
#[path = "native_async.rs"]
mod native_async;
#[path = "numeric.rs"]
mod numeric;
#[path = "numeric_guards.rs"]
mod numeric_guards;
#[path = "operators.rs"]
mod operators;
#[path = "options_hash.rs"]
mod options_hash;
#[path = "ordering.rs"]
mod ordering;
#[path = "output.rs"]
mod output;
#[path = "parse_parity.rs"]
mod parse_parity;
#[path = "parse_recovery.rs"]
mod parse_recovery;
#[path = "parser_boundaries.rs"]
mod parser_boundaries;
#[path = "random.rs"]
mod random;
#[path = "range_aggregates.rs"]
mod range_aggregates;
#[path = "recoverable_mutations.rs"]
mod recoverable_mutations;
#[path = "regex.rs"]
mod regex;
#[path = "regex_values.rs"]
mod regex_values;
#[path = "rendering.rs"]
mod rendering;
#[path = "require.rs"]
mod require;
#[path = "safe_navigation.rs"]
mod safe_navigation;
#[path = "scalar_helpers.rs"]
mod scalar_helpers;
#[path = "scalar_messages.rs"]
mod scalar_messages;
#[path = "shapes.rs"]
mod shapes;
#[path = "site.rs"]
mod site;
#[path = "site_harness.rs"]
mod site_harness;
#[path = "static_types.rs"]
mod static_types;
#[path = "string_case.rs"]
mod string_case;
#[path = "string_charsets.rs"]
mod string_charsets;
#[path = "string_iteration.rs"]
mod string_iteration;
#[path = "string_methods.rs"]
mod string_methods;
#[path = "string_transforms.rs"]
mod string_transforms;
#[path = "substitution.rs"]
mod substitution;
#[path = "syntax_depth.rs"]
mod syntax_depth;
#[path = "time.rs"]
mod time;
#[path = "time_anchors.rs"]
mod time_anchors;
#[path = "time_format.rs"]
mod time_format;
#[path = "time_parse.rs"]
mod time_parse;
#[path = "type_diagnostics.rs"]
mod type_diagnostics;
#[path = "type_literals.rs"]
mod type_literals;
#[path = "typed_declarations.rs"]
mod typed_declarations;
#[path = "types.rs"]
mod types;
#[path = "unary_strings.rs"]
mod unary_strings;
#[path = "unicode.rs"]
mod unicode;
#[path = "upstream.rs"]
mod upstream;
#[path = "upstream_driver.rs"]
mod upstream_driver;
#[path = "value_helpers.rs"]
mod value_helpers;
#[path = "value_semantics.rs"]
mod value_semantics;
#[path = "vm_loops.rs"]
mod vm_loops;
#[path = "vm_snapshots.rs"]
mod vm_snapshots;

#[test]
fn every_root_test_file_is_registered() {
    use std::{collections::BTreeSet, fs, path::Path};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files: BTreeSet<_> = fs::read_dir(root.join("tests"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    let manifest = include_str!("../Cargo.toml");
    let targets = manifest.split("[[test]]").skip(1).filter_map(|section| {
        section
            .lines()
            .take_while(|line| !line.starts_with('['))
            .find_map(|line| line.strip_prefix("path = \"tests/"))
            .and_then(|path| path.strip_suffix('"'))
    });
    let modules = include_str!("all.rs").lines().filter_map(|line| {
        line.strip_prefix("#[path = \"")
            .and_then(|path| path.strip_suffix("\"]"))
    });
    let included: BTreeSet<_> = modules.chain(targets).map(str::to_owned).collect();
    assert_eq!(
        files, included,
        "register each tests/*.rs file in all.rs or an explicit [[test]] target"
    );
}

#[path = "memory_modules.rs"]
mod memory_modules;
#[path = "name_suffix.rs"]
mod name_suffix;
