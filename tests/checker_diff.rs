//! Differential tests of the type checker against the runtime: generated
//! programs the checker accepts must run the same with every proven type
//! check kept, and each reduced finding stays fixed. `examples/checker_diff.rs`
//! runs the long generation; see `docs/checker-diff.md`.

// The test does not describe the signatures of its findings.
#[allow(dead_code)]
#[path = "../examples/checker_diff/builtins.rs"]
mod builtins;
#[path = "../examples/checker_diff/generate.rs"]
mod generate;
// The example's command line uses parts of the harness this test does not.
#[allow(dead_code)]
#[path = "../examples/checker_diff/harness.rs"]
mod harness;
#[path = "../examples/checker_diff/rng.rs"]
mod rng;

use harness::{Case, Scratch, Verdict};
use std::path::PathBuf;

mod common;

fn scratch() -> (PathBuf, Scratch) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cache/tmp")
        .join(format!(
            "checker-diff-{}-{:?}",
            common::process_id(),
            std::thread::current().id()
        ));
    let scratch = Scratch::new(&root);
    (root, scratch)
}

/// Seeds the smoke test judges; a few seconds in a debug build.
const SMOKE_SEEDS: u64 = 300;

#[test]
fn generated_programs_agree_with_their_checks_kept() {
    let (root, mut scratch) = scratch();
    let mut accepted = 0;
    let mut failures = Vec::new();
    for seed in 0..SMOKE_SEEDS {
        let case = generate::program(seed);
        match harness::judge(&case, &mut scratch) {
            Verdict::Finding(finding) if harness::known(&case, &finding).is_some() => accepted += 1,
            Verdict::Finding(finding) => failures.push(format!(
                "seed {seed}: {}\n{}\n{}",
                finding.kind.name(),
                finding.detail,
                case.render()
            )),
            Verdict::Agreed(_) | Verdict::Inconclusive => accepted += 1,
            Verdict::Rejected(_) => (),
        }
    }
    let _ = std::fs::remove_dir_all(root);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    // The generator must keep producing programs the checker accepts.
    assert!(
        accepted * 5 >= SMOKE_SEEDS,
        "only {accepted} of {SMOKE_SEEDS} accepted"
    );
}

/// Each reduced finding in `tests/checker-diff`, whose first line says
/// what the program must now do: `# expect: rejected CODE` or
/// `# expect: agreed`.
#[test]
fn reduced_findings_stay_fixed() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/checker-diff");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "vibe")
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let (root, mut scratch) = scratch();
    let mut failures = Vec::new();
    for path in &paths {
        let text = std::fs::read_to_string(path).unwrap();
        let expected = text
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("# expect: "))
            .unwrap_or_else(|| panic!("{} has no `# expect:` line", path.display()))
            .to_owned();
        let verdict = harness::judge(&Case::parse(&text), &mut scratch);
        let fits = match &verdict {
            Verdict::Rejected(reason) => expected
                .strip_prefix("rejected ")
                .is_some_and(|code| reason.starts_with(code)),
            Verdict::Agreed(_) => expected == "agreed",
            _ => false,
        };
        if !fits {
            failures.push(format!(
                "{}: expected {expected}, got {verdict:?}",
                path.display()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
