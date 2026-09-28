//! Differential tests of the type checker against the runtime.
//!
//! Every program the checker accepts runs twice: as a release build compiles
//! it, and with every type check the checker proves kept
//! (`Engine::set_keep_type_checks`). A kept check that fails, any other
//! difference, an error the checker rules out, a panic or a hang is a
//! checker bug. See `docs/checker-diff.md`.
//!
//! ```sh
//! checker_diff judge FILE...           # judge hand-written programs
//! checker_diff generate SEED           # print one generated program
//! checker_diff run [--from N] [--count M] [--jobs J] [--out DIR] [--source S]
//! checker_diff minimize FILE...        # reduce findings to FILE.min.vibe
//! ```
//!
//! `run` judges the programs of seeds N to N+M-1, writes each finding to
//! DIR, and prints totals. `--source` picks them: `generated` programs,
//! type-changing edits of the `corpus` programs, or both, `mixed`, where a
//! fifth are edits. A program whose check or run takes longer than
//! two minutes is written to DIR as a hang, and the process exits with
//! status 3, so a driver can continue after it.

#[path = "checker_diff/builtins.rs"]
mod builtins;
#[path = "checker_diff/generate.rs"]
mod generate;
#[path = "checker_diff/harness.rs"]
mod harness;
#[path = "checker_diff/host.rs"]
mod host;
#[path = "checker_diff/minimize.rs"]
mod minimize;
#[path = "checker_diff/mutate.rs"]
mod mutate;
#[path = "checker_diff/rng.rs"]
mod rng;

use harness::{Case, Scratch, Verdict};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

fn main() {
    // Deep syntax needs more native stack than the main thread has.
    let worker = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .expect("spawn the worker");
    let code = worker.join().unwrap_or(101);
    std::process::exit(code);
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::panic::set_hook(Box::new(|_| {}));
    match args.first().map(String::as_str) {
        Some("judge") => {
            let mut scratch = Scratch::new(&scratch_root(0));
            for path in &args[1..] {
                let text = std::fs::read_to_string(path).expect("read the program");
                let case = Case::parse(&text);
                let verdict = harness::judge(&case, &mut scratch);
                println!("{path}: {}", describe(&verdict));
            }
            let _ = std::fs::remove_dir_all(scratch_root(0));
            0
        }
        Some("generate") => {
            let seed: u64 = args.get(1).and_then(|seed| seed.parse().ok()).unwrap_or(0);
            let source = args.get(2).map_or("generated", String::as_str);
            print!("{}", case_for(seed, source).render());
            0
        }
        Some("run") => batch(&args[1..]),
        Some("minimize") => {
            let mut scratch = Scratch::new(&scratch_root(0));
            for path in &args[1..] {
                let text = std::fs::read_to_string(path).expect("read the program");
                let case = Case::parse(&text);
                let Verdict::Finding(finding) = harness::judge(&case, &mut scratch) else {
                    println!("{path}: no finding");
                    continue;
                };
                let small = minimize::minimize(&case, &finding, &mut scratch);
                let verdict = harness::judge(&small, &mut scratch);
                let header: String = describe(&verdict)
                    .lines()
                    .map(|line| format!("# {line}\n"))
                    .collect();
                let output = format!("{header}{}", small.render());
                let target = format!("{}.min.vibe", path.trim_end_matches(".vibe"));
                std::fs::write(&target, &output).expect("write the reduced program");
                println!("== {target}\n{output}");
            }
            let _ = std::fs::remove_dir_all(scratch_root(0));
            0
        }
        _ => {
            eprintln!(
                "usage: checker_diff judge FILE... | generate SEED | run [--from N] [--count M] [--jobs J] [--out DIR]"
            );
            2
        }
    }
}

/// The program of `seed` from `source`: `generated`, `corpus` or `mixed`.
fn case_for(seed: u64, source: &str) -> Case {
    let edit = match source {
        "corpus" => true,
        "mixed" => seed % 5 == 4,
        _ => false,
    };
    if edit {
        if let Some(case) = mutate::program(seed) {
            return case;
        }
    }
    generate::program(seed)
}

fn scratch_root(worker: usize) -> PathBuf {
    std::env::temp_dir().join(format!("checker-diff-{}-{worker}", std::process::id()))
}

fn describe(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Rejected(code) => format!("rejected {code}"),
        Verdict::Agreed(result) => format!("agreed {result}"),
        Verdict::Inconclusive => "inconclusive".to_owned(),
        Verdict::Finding(finding) => format!("FINDING {}\n{}", finding.kind.name(), finding.detail),
    }
}

/// Each worker's current seed and when it started, for the watchdog.
type Watch = Arc<Mutex<Vec<Option<(u64, Instant)>>>>;

/// How many programs of each kind of finding a run writes.
const KEEP: u64 = 200;

#[derive(Default)]
struct Totals {
    generated: u64,
    annotated: u64,
    rejected: BTreeMap<String, u64>,
    agreed: u64,
    agreed_errors: BTreeMap<String, u64>,
    inconclusive: u64,
    compared: u64,
    findings: BTreeMap<&'static str, u64>,
}

impl Totals {
    /// Counts `finding`, and returns the name its file takes, or `None`
    /// when enough of its kind were written.
    fn finding(&mut self, finding: &harness::Finding) -> Option<String> {
        let count = self.findings.entry(finding.kind.name()).or_default();
        *count += 1;
        // A frequent finding need not fill the disk.
        (*count <= KEEP).then(|| finding.kind.name().to_owned())
    }
}

fn batch(args: &[String]) -> i32 {
    let option = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let from: u64 = option("--from")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let count: u64 = option("--count")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000);
    let jobs: usize = option("--jobs")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let out = PathBuf::from(option("--out").unwrap_or_else(|| "checker-diff-findings".to_owned()));
    let source = option("--source").unwrap_or_else(|| "generated".to_owned());
    if source != "generated" {
        // Load the corpus once, before the workers need it.
        mutate::corpus();
    }
    std::fs::create_dir_all(&out).expect("create the findings directory");
    let next = Arc::new(AtomicU64::new(from));
    let end = from + count;
    let totals = Arc::new(Mutex::new(Totals::default()));
    let current: Watch = Arc::new(Mutex::new(vec![None; jobs]));
    let started = Instant::now();
    let mut handles = Vec::new();
    for worker in 0..jobs {
        let next = next.clone();
        let totals = totals.clone();
        let current = current.clone();
        let out = out.clone();
        let source = source.clone();
        let handle = std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let root = scratch_root(worker + 1);
                let mut scratch = Scratch::new(&root);
                loop {
                    let seed = next.fetch_add(1, Ordering::Relaxed);
                    if seed >= end {
                        break;
                    }
                    current.lock().unwrap()[worker] = Some((seed, Instant::now()));
                    let case = case_for(seed, &source);
                    let verdict = harness::judge(&case, &mut scratch);
                    record(&totals, &out, seed, &case, &verdict);
                    if matches!(verdict, Verdict::Agreed(_)) {
                        annotated(&totals, &out, seed, &case, &mut scratch);
                    }
                }
                current.lock().unwrap()[worker] = None;
                let _ = std::fs::remove_dir_all(root);
            })
            .expect("spawn a worker");
        handles.push(handle);
    }
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if handles.iter().all(|handle| handle.is_finished()) {
            break;
        }
        let stuck = current
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .find(|(_, since)| since.elapsed() > Duration::from_secs(120))
            .copied();
        if let Some((seed, _)) = stuck {
            let case = case_for(seed, &source);
            let path = out.join(format!("hang-{seed}.vibe"));
            let _ = std::fs::write(&path, format!("# hang: seed {seed}\n{}", case.render()));
            eprintln!(
                "seed {seed} did not finish in two minutes; wrote {}",
                path.display()
            );
            summary(&totals.lock().unwrap(), started, &out);
            std::process::exit(3);
        }
    }
    for handle in handles {
        let _ = handle.join();
    }
    summary(&totals.lock().unwrap(), started, &out);
    0
}

fn record(totals: &Mutex<Totals>, out: &Path, seed: u64, case: &Case, verdict: &Verdict) {
    let mut totals = totals.lock().unwrap();
    totals.generated += 1;
    match verdict {
        Verdict::Rejected(reason) => {
            let code = reason.split(' ').next().unwrap_or_default().to_owned();
            *totals.rejected.entry(code.clone()).or_default() += 1;
            if std::env::var_os("CHECKER_DIFF_REJECTED").is_some() {
                let path = out.join(format!("rejected-{code}-{seed}.vibe"));
                let _ = std::fs::write(path, format!("# {reason}\n{}", case.render()));
            }
        }
        Verdict::Agreed(result) => {
            totals.agreed += 1;
            totals.compared += 1;
            if let Some(error) = result.strip_prefix("error ") {
                let key: String = error
                    .split(": ")
                    .nth(1)
                    .unwrap_or(error)
                    .chars()
                    .take(60)
                    .collect();
                let kind = error.split(' ').next().unwrap_or_default();
                *totals
                    .agreed_errors
                    .entry(format!("{kind}: {key}"))
                    .or_default() += 1;
            }
        }
        Verdict::Inconclusive => totals.inconclusive += 1,
        Verdict::Finding(finding) => {
            totals.compared += 1;
            let Some(name) = totals.finding(finding) else {
                return;
            };
            let path = out.join(format!("{name}-{seed}.vibe"));
            let header: String = finding
                .detail
                .lines()
                .map(|line| format!("# {line}\n"))
                .collect();
            let _ = std::fs::write(path, format!("# seed {seed}\n{header}{}", case.render()));
        }
    }
}

/// Judges `case` again with its top-level locals declared with the types
/// the checker inferred. The checker must accept its own inferences.
fn annotated(totals: &Mutex<Totals>, out: &Path, seed: u64, case: &Case, scratch: &mut Scratch) {
    let Some(annotated) = harness::annotate(case, scratch) else {
        return;
    };
    let verdict = harness::judge(&annotated, scratch);
    let mut counts = totals.lock().unwrap();
    counts.annotated += 1;
    match &verdict {
        Verdict::Finding(finding) => {
            let Some(name) = counts.finding(finding) else {
                return;
            };
            let path = out.join(format!("annotated-{name}-{seed}.vibe"));
            let header: String = finding
                .detail
                .lines()
                .map(|line| format!("# {line}\n"))
                .collect();
            let _ = std::fs::write(
                path,
                format!("# seed {seed}, annotated\n{header}{}", annotated.render()),
            );
        }
        Verdict::Rejected(reason) => {
            let count = counts.findings.entry("annotation-rejected").or_default();
            *count += 1;
            if *count > KEEP {
                return;
            }
            let path = out.join(format!("annotation-rejected-{seed}.vibe"));
            let _ = std::fs::write(
                path,
                format!("# seed {seed}: {reason}\n{}", annotated.render()),
            );
        }
        _ => {}
    }
}

fn summary(totals: &Totals, started: Instant, out: &Path) {
    let rejected: u64 = totals.rejected.values().sum();
    let findings: u64 = totals.findings.values().sum();
    println!(
        "generated {} accepted {} compared {} (agreed {}, inconclusive {}, annotated {}) rejected {} findings {} in {:.1}s",
        totals.generated,
        totals.generated - rejected,
        totals.compared,
        totals.agreed,
        totals.inconclusive,
        totals.annotated,
        rejected,
        findings,
        started.elapsed().as_secs_f64()
    );
    for (kind, count) in &totals.findings {
        println!("  finding {kind}: {count}");
    }
    let mut codes: Vec<(&String, &u64)> = totals.rejected.iter().collect();
    codes.sort_by(|a, b| b.1.cmp(a.1));
    let codes: Vec<String> = codes
        .iter()
        .map(|(code, count)| format!("{code} {count}"))
        .collect();
    println!("  rejections: {}", codes.join(", "));
    let mut errors: Vec<(&String, &u64)> = totals.agreed_errors.iter().collect();
    errors.sort_by(|a, b| b.1.cmp(a.1));
    for (error, count) in errors.iter().take(25) {
        println!("  agreed error {count}: {error}");
    }
    println!("  findings in {}", out.display());
}
