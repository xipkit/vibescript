//! Compares `vibes fix` with `vibes migrate` on the canonical surface: for
//! each old-language source, the text the compiler's fixes reach round by
//! round against the text the migration's surface rewrites reach at once.
//!
//! Run over an exported corpus tree (`scripts/golden.py --export DIR`):
//!
//! ```sh
//! CROSSCHECK_DIR=DIR CROSSCHECK_REPORT=report.txt \
//!     ./scripts/cargo test --release -p vibescript-tools --test crosscheck -- --ignored
//! ```

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript_tools::{
    fix::fix,
    migrate::{Observations, Options, migrate},
};

/// What the two tools did to one source.
enum Outcome {
    Same,
    /// Neither changed it.
    Untouched,
    /// The migration's single pass stopped at a spelling one of its own
    /// rewrites produced, such as `x.send(:clone)` becoming `x.clone`, and
    /// fixing its output reaches the fixed text.
    Completes,
    Different(String, String),
    /// The compiler does not parse it, or the migration left it unparsed.
    Skipped,
}

fn compare(source: &str) -> Outcome {
    let engine = vibescript::Engine::new();
    let check = |text: &str| engine.type_check(text).map(|checked| checked.diagnostics);
    let Ok(fixed) = fix(source, check) else {
        return Outcome::Skipped;
    };
    let options = Options {
        surface_only: true,
        ..Options::default()
    };
    let migrated = migrate(source, &Observations::default(), &options);
    if migrated
        .diagnostics
        .iter()
        .any(|d| matches!(d.code.id(), "unparsed" | "internal"))
    {
        return Outcome::Skipped;
    }
    if fixed.source == migrated.source {
        if fixed.source == source {
            Outcome::Untouched
        } else {
            Outcome::Same
        }
    } else if fix(&migrated.source, check).is_ok_and(|again| again.source == fixed.source) {
        Outcome::Completes
    } else {
        Outcome::Different(fixed.source, migrated.source)
    }
}

fn sources(directory: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            sources(&entry, out);
        } else if entry.extension().is_some_and(|e| e == "vibe") {
            out.push(entry);
        }
    }
}

#[test]
#[ignore]
fn fix_matches_the_migrations_surface_rewrites() {
    let directory = PathBuf::from(std::env::var("CROSSCHECK_DIR").expect("CROSSCHECK_DIR"));
    let report = std::env::var("CROSSCHECK_REPORT").expect("CROSSCHECK_REPORT");
    let mut files = Vec::new();
    sources(&directory, &mut files);
    let next = AtomicUsize::new(0);
    let counts = Mutex::new(BTreeMap::<&str, usize>::new());
    let differences = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()) {
            std::thread::Builder::new()
                .stack_size(256 << 20)
                .spawn_scoped(scope, || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = files.get(index) else {
                            break;
                        };
                        let Ok(source) = std::fs::read_to_string(path) else {
                            continue;
                        };
                        let outcome = compare(&source);
                        let kind = match &outcome {
                            Outcome::Same => "same",
                            Outcome::Untouched => "untouched",
                            Outcome::Completes => "completes",
                            Outcome::Different(..) => "different",
                            Outcome::Skipped => "skipped",
                        };
                        *counts.lock().unwrap().entry(kind).or_default() += 1;
                        if let Outcome::Different(fixed, migrated) = outcome {
                            let name = path.strip_prefix(&directory).unwrap_or(path);
                            differences.lock().unwrap().push((
                                name.display().to_string(),
                                source,
                                fixed,
                                migrated,
                            ));
                        }
                    }
                })
                .unwrap();
        }
    });
    let counts = counts.into_inner().unwrap();
    let mut differences = differences.into_inner().unwrap();
    differences.sort();
    let mut text = format!("{counts:?}\n");
    for (name, source, fixed, migrated) in &differences {
        text.push_str(&format!(
            "=== {name}\n--- source\n{source}--- fix\n{fixed}--- migrate\n{migrated}\n"
        ));
    }
    std::fs::write(&report, text).unwrap();
    eprintln!("{counts:?}");
}
