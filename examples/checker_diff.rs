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
//! checker_diff verdicts [--from N] [--count M] [--jobs J] [--source S] > FILE
//! checker_diff rejections BEFORE AFTER [--source S] [--out DIR]
//! ```
//!
//! `run` judges the programs of seeds N to N+M-1, writes each finding to
//! DIR, and prints totals. `--source` picks them: `generated` programs,
//! type-changing edits of the `corpus` programs, or both, `mixed`, where a
//! fifth are edits, or `files`, programs of a required file whose locals
//! its functions' parameters shadow. A program whose check or run takes
//! longer than two minutes is written to DIR as a hang, and the process
//! exits with status 3, so a driver can continue after it.
//!
//! `verdicts` prints how a build takes each program, a line a seed:
//! whether the checker rejects it, with the code, or how running it, with
//! every check kept, ends. `rejections` compares two such files, from
//! builds of two versions of the checker, and writes to DIR each program
//! the second rejects that the first accepts and runs without an error.

#[path = "checker_diff/builtins.rs"]
mod builtins;
#[path = "checker_diff/files.rs"]
mod files;
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
        Some("verdicts") => verdicts(&args[1..], case_for),
        Some("rejections") => rejections(&args[1..]),
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

/// The value of option `name` in `args`.
fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// Claims a seed below `end`, leaving an exhausted counter unchanged.
fn next_seed(next: &AtomicU64, end: u64) -> Option<u64> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |seed| {
        (seed < end).then(|| seed + 1)
    })
    .ok()
}

/// The positive number of workers shared by both differential commands.
fn worker_count(args: &[String]) -> usize {
    let jobs = option(args, "--jobs")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    assert!(jobs > 0, "differential runs need at least one worker");
    jobs
}

/// Prints how a build takes the programs of seeds N to N+M-1, a line a
/// seed in the order they finish: the seed, then `rejected CODE`, or how
/// running it with every check kept ended: `ran`, `failed`, `limited` or
/// `panicked`. A program whose check or run takes longer than two minutes
/// ends the run with status 3.
fn verdicts(args: &[String], make_case: fn(u64, &str) -> Case) -> i32 {
    use std::io::Write;
    let from: u64 = option(args, "--from")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let count: u64 = option(args, "--count")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000);
    let jobs = worker_count(args);
    let source = option(args, "--source").unwrap_or_else(|| "generated".to_owned());
    if matches!(source.as_str(), "corpus" | "mixed") {
        mutate::corpus();
    }
    let next = Arc::new(AtomicU64::new(from));
    let end = from
        .checked_add(count)
        .expect("verdict seed range overflows u64");
    let output = Arc::new(Mutex::new(std::io::BufWriter::new(std::io::stdout())));
    let current: Watch = Arc::new(Mutex::new(vec![None; jobs]));
    let mut handles = Vec::new();
    for worker in 0..jobs {
        let (next, output, current, source) = (
            next.clone(),
            output.clone(),
            current.clone(),
            source.clone(),
        );
        let handle = std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let root = scratch_root(worker + 1);
                let mut scratch = Scratch::new(&root);
                while let Some(seed) = next_seed(&next, end) {
                    current.lock().unwrap()[worker] = Some((seed, Instant::now()));
                    let case = make_case(seed, &source);
                    let outcome = harness::outcome(&case, &mut scratch);
                    writeln!(output.lock().unwrap(), "{seed} {}", outcome.line())
                        .expect("write a verdict");
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
            let _ = output.lock().unwrap().flush();
            eprintln!("seed {seed} did not finish in two minutes");
            std::process::exit(3);
        }
    }
    for handle in handles {
        handle.join().expect("verdict worker panicked");
    }
    output.lock().unwrap().flush().expect("flush verdicts");
    0
}

/// Compares two files of [`verdicts`], from builds of two versions of the
/// checker, by seed: prints how many seeds went from each outcome to each
/// other, and writes to DIR, up to [`KEEP`] of each kind, each program the
/// second rejects that the first accepts and runs without an error. Run by
/// a build of the second version, it checks each such program again and
/// sorts them by its first error's message, with the names in it left out.
fn rejections(args: &[String]) -> i32 {
    let read = |path: &String| -> BTreeMap<u64, String> {
        let text = std::fs::read_to_string(path).expect("read a verdicts file");
        text.lines()
            .filter_map(|line| {
                let (seed, outcome) = line.split_once(' ')?;
                Some((seed.parse().ok()?, outcome.to_owned()))
            })
            .collect()
    };
    let (Some(before), Some(after)) = (args.first(), args.get(1)) else {
        eprintln!("usage: checker_diff rejections BEFORE AFTER [--source S] [--out DIR]");
        return 2;
    };
    let (before, after) = (read(before), read(after));
    let source = option(args, "--source").unwrap_or_else(|| "generated".to_owned());
    let out =
        PathBuf::from(option(args, "--out").unwrap_or_else(|| "checker-rejections".to_owned()));
    std::fs::create_dir_all(&out).expect("create the rejections directory");
    if matches!(source.as_str(), "corpus" | "mixed") {
        mutate::corpus();
    }
    let mut changes: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut rejected: BTreeMap<String, u64> = BTreeMap::new();
    let mut scratch = Scratch::new(&scratch_root(0));
    let mut compared = 0;
    for (seed, first) in &before {
        let Some(second) = after.get(seed) else {
            continue;
        };
        compared += 1;
        if first == second {
            continue;
        }
        let kind = |outcome: &String| outcome.split(' ').next().unwrap_or_default().to_owned();
        *changes.entry((kind(first), kind(second))).or_default() += 1;
        let Some(code) = second.strip_prefix("rejected ") else {
            continue;
        };
        if first != "ran" {
            continue;
        }
        let case = case_for(*seed, &source);
        let message = match harness::outcome(&case, &mut scratch) {
            harness::Outcome::Rejected(reason) => reason,
            other => format!("{code}: {}", other.line()),
        };
        let kind = format!(
            "{code} {}",
            unnamed(
                message
                    .split(": ")
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(": ")
                    .as_str()
            )
        );
        let count = rejected.entry(kind).or_default();
        *count += 1;
        if *count <= KEEP {
            let path = out.join(format!("{code}-{seed}.vibe"));
            let header = format!("# seed {seed}: ran before, rejected after: {message}\n");
            let _ = std::fs::write(path, format!("{header}{}", case.render()));
        }
    }
    let _ = std::fs::remove_dir_all(scratch_root(0));
    println!("compared {compared} seeds");
    for ((first, second), count) in &changes {
        println!("  {first} -> {second}: {count}");
    }
    let mut kinds: Vec<(&String, &u64)> = rejected.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    for (kind, count) in kinds {
        println!("  ran before, rejected after, {count}: {kind}");
    }
    println!("  programs in {}", out.display());
    0
}

/// `message` with each name in backticks, and each number, left out, so
/// that messages of one kind read alike.
fn unnamed(message: &str) -> String {
    let mut text = String::new();
    let mut quoted = false;
    for c in message.chars() {
        if c == '`' {
            quoted = !quoted;
            if !quoted {
                text.push_str("`_`");
            }
        } else if !quoted && !c.is_ascii_digit() {
            text.push(c);
        }
    }
    text.chars().take(160).collect()
}

/// The program of `seed` from `source`: `generated`, `corpus` or `mixed`,
/// or `files`, a required file whose locals its functions' parameters
/// shadow.
fn case_for(seed: u64, source: &str) -> Case {
    if source == "files" {
        return files::program(seed);
    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn batch_rejects_zero_workers_before_creating_output() {
        let out = super::scratch_root(usize::MAX).join("zero-workers");
        let args = vec![
            "--count".to_owned(),
            "1".to_owned(),
            "--jobs".to_owned(),
            "0".to_owned(),
            "--out".to_owned(),
            out.to_str().unwrap().to_owned(),
        ];
        assert!(!out.exists());
        assert!(std::panic::catch_unwind(|| super::batch(&args)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn seed_claims_do_not_wrap_at_the_end_of_u64() {
        let next = super::AtomicU64::new(u64::MAX - 1);
        std::thread::scope(|scope| {
            let claims: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| super::next_seed(&next, u64::MAX)))
                .collect();
            let seeds: Vec<_> = claims
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect();
            assert_eq!(seeds, [u64::MAX - 1]);
        });
        assert_eq!(super::next_seed(&next, u64::MAX), None);
        assert_eq!(next.load(super::Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn verdicts_reject_zero_workers() {
        let args = ["--count", "1", "--jobs", "0"].map(str::to_owned);
        assert!(std::panic::catch_unwind(|| super::verdicts(&args, super::case_for)).is_err());
    }

    #[test]
    fn verdicts_reject_overflowing_seed_ranges() {
        let args = [
            "--from",
            "18446744073709551615",
            "--count",
            "1",
            "--jobs",
            "1",
        ]
        .map(str::to_owned);
        assert!(std::panic::catch_unwind(|| super::verdicts(&args, super::case_for)).is_err());
    }

    #[test]
    fn verdicts_fail_when_generating_a_case_panics() {
        let args = ["--count", "1", "--jobs", "1"].map(str::to_owned);
        let result = std::panic::catch_unwind(|| {
            super::verdicts(&args, |_, _| panic!("case generation failed"))
        });
        assert!(
            result.is_err(),
            "a truncated verdict file must fail the command"
        );
    }
}

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
    let jobs = worker_count(args);
    let out = PathBuf::from(option("--out").unwrap_or_else(|| "checker-diff-findings".to_owned()));
    let source = option("--source").unwrap_or_else(|| "generated".to_owned());
    if matches!(source.as_str(), "corpus" | "mixed") {
        // Load the corpus once, before the workers need it.
        mutate::corpus();
    }
    let end = from
        .checked_add(count)
        .expect("batch seed range overflows u64");
    std::fs::create_dir_all(&out).expect("create the findings directory");
    let next = Arc::new(AtomicU64::new(from));
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
                while let Some(seed) = next_seed(&next, end) {
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
