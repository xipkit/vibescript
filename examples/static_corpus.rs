//! Type checks the migrated golden corpora with the static checker.
//!
//! Usage: `static_corpus [WORK] [--corpus a,b] [--out FILE] [--examples N]`
//!
//! WORK is the full migration's tree, `.cache/migrate/work/full` by default,
//! as `scripts/migrate-corpora.py` writes it. Every source the migrator
//! rewrote without a manual diagnostic is checked; the summary counts, per
//! corpus, the sources and cases that check clean and the diagnostics of the
//! others by code, with examples. `--out` writes every diagnostic as JSON.
//! `scripts/static-corpus.py` builds and runs this.
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vibescript::{Engine, ModuleConfig};

const CORPORA: [&str; 5] = [
    "conformance",
    "language",
    "rejections",
    "compatibility",
    "replay",
];

struct Outcome {
    file: String,
    bytes: usize,
    elapsed: Duration,
    /// Each error as `(code, line, column, message)`, or the parse error.
    errors: Vec<(String, usize, usize, String)>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut work = PathBuf::from(".cache/migrate/work/full");
    let mut corpora: Vec<String> = CORPORA.iter().map(|c| c.to_string()).collect();
    let mut out = None;
    let mut examples = 5;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--corpus" => {
                index += 1;
                corpora = args[index].split(',').map(str::to_owned).collect();
            }
            "--out" => {
                index += 1;
                out = Some(PathBuf::from(&args[index]));
            }
            "--examples" => {
                index += 1;
                examples = args[index].parse().expect("--examples takes a number");
            }
            path => work = PathBuf::from(path),
        }
        index += 1;
    }
    if !work.exists() {
        eprintln!(
            "static_corpus: {} does not exist; run scripts/migrate-corpora.py first",
            work.display()
        );
        std::process::exit(2);
    }
    let mut summary = serde_json::Map::new();
    let mut details = serde_json::Map::new();
    let mut total_time = Duration::ZERO;
    let mut largest: Option<(usize, String, Duration)> = None;
    let mut failed = false;
    for corpus in &corpora {
        let report = work.join(format!("{corpus}.report.json"));
        let Ok(text) = fs::read_to_string(&report) else {
            eprintln!("static_corpus: skipping {corpus}: no {}", report.display());
            continue;
        };
        let entries: Vec<Json> = serde_json::from_str(&text).expect("report is JSON");
        let automatic: Vec<String> = entries
            .iter()
            .filter(|entry| entry["diagnostics"].as_array().is_some_and(Vec::is_empty))
            .map(|entry| entry["file"].as_str().unwrap().to_owned())
            .collect();
        let manual: BTreeSet<String> = entries
            .iter()
            .filter(|entry| !entry["diagnostics"].as_array().is_some_and(Vec::is_empty))
            .map(|entry| entry["file"].as_str().unwrap().to_owned())
            .collect();
        let outcomes = check_all(&work.join(corpus), &automatic);
        let mut clean = 0;
        let mut by_code: BTreeMap<String, (usize, Vec<Json>)> = BTreeMap::new();
        let mut failing = serde_json::Map::new();
        let mut errors_by_file: BTreeMap<&str, bool> = BTreeMap::new();
        for outcome in &outcomes {
            total_time += outcome.elapsed;
            if largest
                .as_ref()
                .is_none_or(|(bytes, _, _)| outcome.bytes > *bytes)
            {
                largest = Some((
                    outcome.bytes,
                    format!("{corpus}/{}", outcome.file),
                    outcome.elapsed,
                ));
            }
            errors_by_file.insert(&outcome.file, outcome.errors.is_empty());
            if outcome.errors.is_empty() {
                clean += 1;
                continue;
            }
            let mut codes = BTreeSet::new();
            for (code, line, column, message) in &outcome.errors {
                codes.insert(code.clone());
                let entry = by_code.entry(code.clone()).or_default();
                if entry.1.len() < examples && !entry.1.iter().any(|e| e["file"] == outcome.file) {
                    entry.1.push(json!({
                        "file": outcome.file,
                        "at": format!("{line}:{column}"),
                        "message": message,
                    }));
                }
            }
            for code in codes {
                by_code.get_mut(&code).unwrap().0 += 1;
            }
            failing.insert(
                outcome.file.clone(),
                Json::Array(
                    outcome
                        .errors
                        .iter()
                        .map(|(code, line, column, message)| json!([code, line, column, message]))
                        .collect(),
                ),
            );
        }
        let (cases, clean_cases) = cases(&work.join(corpus), &errors_by_file, &manual);
        println!(
            "{corpus:14} automatic sources {:>7}  clean {:>7}  failing {:>6}   automatic cases {:>7}  clean {:>7}",
            outcomes.len(),
            clean,
            outcomes.len() - clean,
            cases,
            clean_cases
        );
        for (code, (count, _)) in &by_code {
            println!("{:14}   {code}: {count} sources", "");
        }
        failed |= clean != outcomes.len();
        summary.insert(
            corpus.clone(),
            json!({
                "automatic_sources": outcomes.len(),
                "clean_sources": clean,
                "automatic_cases": cases,
                "clean_cases": clean_cases,
                "by_code": by_code
                    .iter()
                    .map(|(code, (count, samples))| (code.clone(), json!({"sources": count, "examples": samples})))
                    .collect::<serde_json::Map<_, _>>(),
            }),
        );
        details.insert(corpus.clone(), Json::Object(failing));
    }
    println!(
        "checked in {:.2}s of single-threaded checking time",
        total_time.as_secs_f64()
    );
    if let Some((bytes, file, elapsed)) = &largest {
        println!(
            "largest source: {file}, {bytes} bytes, {:.2}ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    summary.insert(
        "time".into(),
        json!({
            "total_seconds": total_time.as_secs_f64(),
            "largest": largest.map(|(bytes, file, elapsed)| json!({"file": file, "bytes": bytes, "ms": elapsed.as_secs_f64() * 1000.0})),
        }),
    );
    let summary = Json::Object(summary);
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if let Some(out) = out {
        let all = json!({"summary": summary, "failures": Json::Object(details)});
        fs::write(&out, serde_json::to_string_pretty(&all).unwrap() + "\n").expect("write --out");
    }
    std::process::exit(i32::from(failed));
}

/// Checks each source on as many threads as there are cores.
fn check_all(root: &Path, files: &[String]) -> Vec<Outcome> {
    let queue = Arc::new(Mutex::new(files.to_vec()));
    let results = Arc::new(Mutex::new(Vec::with_capacity(files.len())));
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let queue = queue.clone();
            let results = results.clone();
            let root = root.to_owned();
            std::thread::Builder::new()
                .stack_size(256 << 20)
                .spawn(move || {
                    let plain = Engine::new();
                    loop {
                        let Some(file) = queue.lock().unwrap().pop() else {
                            break;
                        };
                        let path = root.join(&file);
                        let source = fs::read_to_string(&path).unwrap_or_default();
                        // A case's required files sit beside it, in `<name>.files/`.
                        let modules = path.with_extension("files");
                        let with_modules = modules.is_dir().then(|| {
                            let mut engine = Engine::new();
                            engine
                                .set_module_config(ModuleConfig {
                                    paths: vec![modules],
                                    ..ModuleConfig::default()
                                })
                                .map(|()| engine)
                        });
                        let engine = match &with_modules {
                            Some(Ok(engine)) => engine,
                            _ => &plain,
                        };
                        let start = Instant::now();
                        let checked =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                engine.type_check(&source)
                            }));
                        let Ok(checked) = checked else {
                            results.lock().unwrap().push(Outcome {
                                file,
                                bytes: source.len(),
                                elapsed: start.elapsed(),
                                errors: vec![(
                                    "panic".to_owned(),
                                    0,
                                    0,
                                    "the checker panicked".to_owned(),
                                )],
                            });
                            continue;
                        };
                        let errors = match checked {
                            Ok(checked) => checked
                                .diagnostics
                                .iter()
                                .filter(|d| d.is_error())
                                .map(|d| {
                                    let position = d.span.position(&source);
                                    (
                                        d.code.to_string(),
                                        position.line,
                                        position.column,
                                        d.message.clone(),
                                    )
                                })
                                .collect(),
                            Err(error) => vec![("parse".to_owned(), 0, 0, error.message.clone())],
                        };
                        let elapsed = start.elapsed();
                        results.lock().unwrap().push(Outcome {
                            file,
                            bytes: source.len(),
                            elapsed,
                            errors,
                        });
                    }
                })
                .expect("spawn a checker thread")
        })
        .collect();
    for worker in workers {
        worker.join().expect("join a checker thread");
    }
    let mut results = Arc::try_unwrap(results).ok().unwrap().into_inner().unwrap();
    results.sort_by(|a, b| a.file.cmp(&b.file));
    results
}

/// Counts the cases whose every source is automatic, and those that also
/// check clean, from the tree's index of source keys to files.
fn cases(root: &Path, clean: &BTreeMap<&str, bool>, manual: &BTreeSet<String>) -> (usize, usize) {
    let Ok(text) = fs::read_to_string(root.join("index.json")) else {
        return (clean.len(), clean.values().filter(|c| **c).count());
    };
    let index: BTreeMap<String, String> = serde_json::from_str(&text).expect("index is JSON");
    let mut by_case: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (key, file) in &index {
        let case = key.split("::").next().unwrap();
        by_case.entry(case).or_default().push(file);
    }
    let mut automatic = 0;
    let mut passing = 0;
    for files in by_case.values() {
        if files.iter().any(|file| manual.contains(*file)) {
            continue;
        }
        automatic += 1;
        if files
            .iter()
            .all(|file| clean.get(file).copied().unwrap_or(false))
        {
            passing += 1;
        }
    }
    (automatic, passing)
}
