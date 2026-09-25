//! `vibes migrate`: rewrite ADR-004 Vibescript into the ADR-007 and ADR-008
//! language with `vibescript_tools::migrate`.
//!
//! Every recorded invocation from `--inputs` runs first, with observation,
//! so a file that another requires gets the types its callers passed. Each
//! file is then migrated on its own. Without `--write` the changes print as
//! a unified diff; diagnostics go to stderr, or as JSON to stdout with
//! `--report json`.

use crate::{
    compat,
    flags::{self, Flag, Kind, Outcome, Spec},
    output::Sink,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript_tools::migrate::{self, Invocation, Migration, Observations, Options};

const FLAGS: [Flag; 5] = [
    Flag::new(
        &["write"],
        Kind::Bool,
        "write migrated sources back instead of printing a diff",
    ),
    Flag::new(
        &["inputs"],
        Kind::String,
        "a JSON Lines file of recorded invocations whose observed types annotate the code",
    ),
    Flag::new(
        &["report"],
        Kind::String,
        "print diagnostics as \"text\" (the default, on stderr) or \"json\" (on stdout)",
    ),
    Flag::new(
        &["compatible"],
        Kind::Bool,
        "make only the changes today's runtime accepts",
    ),
    Flag::new(
        &["no-repair"],
        Kind::Bool,
        "skip repairing the migration with the static checker's diagnostics",
    ),
];

pub const SPEC: Spec = Spec {
    name: "migrate",
    aliases: &[],
    usage: "rewrite scripts into the statically typed, canonical language",
    arguments: "<file or directory>...",
    usage_lines: &[],
    flags: &FLAGS,
};

/// Runs `vibes migrate` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, &flags_first(args))? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    if flags.positionals.is_empty() {
        return Err("vibes migrate: file or directory required".to_owned());
    }
    let json = match flags.string("report").as_deref() {
        None | Some("text") => false,
        Some("json") => true,
        Some(other) => {
            return Err(format!(
                "vibes migrate: unknown report format {other:?}; use text or json"
            ));
        }
    };
    let options = Options {
        new_syntax: !flags.bool("compatible"),
        ..Options::default()
    };
    let mut files = Vec::new();
    for positional in &flags.positionals {
        let path = PathBuf::from(positional);
        collect(&path, &path, &mut files)
            .map_err(|error| format!("collect {}: {error}", path.display()))?;
    }
    let invocations = match flags.string("inputs") {
        Some(inputs) => Invocation::read(Path::new(&inputs))?,
        None => Vec::new(),
    };
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|(path, label)| {
            std::fs::read_to_string(path)
                .map(|text| (label.clone(), text))
                .map_err(|error| format!("read {}: {}", path.display(), compat::reason(&error)))
        })
        .collect::<Result<_, _>>()?;
    let mut observations = observe_all(&sources, &invocations);
    // Files an unreproducible run requires may be missing what it passes them.
    for invocation in invocations.iter().filter(|i| !i.reproducible()) {
        for directory in invocation.module_paths() {
            for ((path, _), (_, source)) in files.iter().zip(&sources) {
                if path.starts_with(&directory) {
                    observations.distrust(source);
                }
            }
        }
    }
    let mut migrations = migrate_all(&sources, &observations, &options);
    if options.new_syntax && !flags.bool("no-repair") {
        // A file checks against the migrated files it requires.
        if flags.bool("write") {
            write_changed(&files, &migrations)?;
        }
        migrations = repair_all(&sources, &migrations, &invocations, &observations);
    }
    let out = Sink::Stdout;
    let mut reports = Vec::new();
    let mut text = String::new();
    for ((path, _), ((label, original), migration)) in
        files.iter().zip(sources.iter().zip(&migrations))
    {
        if flags.bool("write") && migration.changed {
            std::fs::write(path, &migration.source)
                .map_err(|error| format!("write {}: {}", path.display(), compat::reason(&error)))?;
        } else if !json && !flags.bool("write") {
            text.push_str(&migrate::unified_diff(label, original, &migration.source));
        }
        if json {
            reports.push((label.as_str(), migration));
        } else {
            for diagnostic in &migration.diagnostics {
                eprintln!(
                    "{label}:{}:{}: {}: {}",
                    diagnostic.line,
                    diagnostic.column,
                    diagnostic.code.id(),
                    diagnostic.message
                );
            }
        }
    }
    if json {
        return out
            .write(migrate::report_json(&reports).as_bytes())
            .map_err(|error| format!("write report: {error}"));
    }
    out.write(text.as_bytes())
        .map_err(|error| format!("write diff: {error}"))
}

/// Moves flags ahead of the paths, so they may follow them as well.
fn flags_first(args: &[OsString]) -> Vec<OsString> {
    let mut flags = Vec::new();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_string_lossy();
        if arg == "--" {
            paths.extend(args[index + 1..].iter().cloned());
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            flags.push(args[index].clone());
            let name = arg.trim_start_matches('-');
            if matches!(name, "inputs" | "report") && index + 1 < args.len() {
                flags.push(args[index + 1].clone());
                index += 1;
            }
        } else {
            paths.push(args[index].clone());
        }
        index += 1;
    }
    flags.push(OsString::from("--"));
    flags.extend(paths);
    flags
}

/// Lists the `.vibe` files under `path` in order, with their names relative to `root`.
pub(crate) fn collect(
    root: &Path,
    path: &Path,
    out: &mut Vec<(PathBuf, String)>,
) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;
        entries.sort();
        for entry in entries {
            if entry.is_dir() || entry.extension().is_some_and(|e| e == "vibe") {
                collect(root, &entry, out)?;
            }
        }
        return Ok(());
    }
    let label = if root.is_dir() {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        path.to_string_lossy().into_owned()
    };
    out.push((path.to_owned(), label));
    Ok(())
}

fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

fn write_changed(files: &[(PathBuf, String)], migrations: &[Migration]) -> Result<(), String> {
    for ((path, _), migration) in files.iter().zip(migrations) {
        if migration.changed {
            std::fs::write(path, &migration.source)
                .map_err(|error| format!("write {}: {}", path.display(), compat::reason(&error)))?;
        }
    }
    Ok(())
}

/// Each file's invocations: those that name it, and those that name none.
struct Runs {
    by_file: std::collections::HashMap<String, Vec<Invocation>>,
    everywhere: Vec<Invocation>,
}

impl Runs {
    fn new(invocations: &[Invocation]) -> Self {
        let mut runs = Self {
            by_file: std::collections::HashMap::new(),
            everywhere: Vec::new(),
        };
        for invocation in invocations {
            match &invocation.file {
                Some(file) => runs
                    .by_file
                    .entry(file.clone())
                    .or_default()
                    .push(invocation.clone()),
                None => runs.everywhere.push(invocation.clone()),
            }
        }
        runs
    }

    fn of(&self, label: &str) -> Vec<Invocation> {
        let mut runs = self.everywhere.clone();
        runs.extend(self.by_file.get(label).into_iter().flatten().cloned());
        runs
    }
}

/// Runs every file's invocations with observation.
fn observe_all(sources: &[(String, String)], invocations: &[Invocation]) -> Observations {
    if invocations.is_empty() {
        return Observations::default();
    }
    let runs = Runs::new(invocations);
    let merged = Mutex::new(Observations::default());
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers() {
            // Recorded runs may recurse as deeply as their limits allow.
            let worker = std::thread::Builder::new().stack_size(1 << 30);
            let spawned = worker.spawn_scoped(scope, || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((label, source)) = sources.get(index) else {
                        break;
                    };
                    let runs = runs.of(label);
                    if runs.is_empty() {
                        continue;
                    }
                    let observed = migrate::observe(source, &runs);
                    merged.lock().unwrap().merge(observed);
                }
            });
            spawned.expect("spawn an observation worker");
        }
    });
    merged.into_inner().unwrap()
}

fn migrate_all(
    sources: &[(String, String)],
    observations: &Observations,
    options: &Options,
) -> Vec<Migration> {
    let results: Vec<Mutex<Option<Migration>>> = sources.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers() {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((_, source)) = sources.get(index) else {
                        break;
                    };
                    let migration = migrate::migrate(source, observations, options);
                    *results[index].lock().unwrap() = Some(migration);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|slot| slot.into_inner().unwrap().unwrap())
        .collect()
}

/// Repairs every migration with the static checker's diagnostics, keeping
/// what each file's invocations do.
fn repair_all(
    sources: &[(String, String)],
    migrations: &[Migration],
    invocations: &[Invocation],
    observations: &Observations,
) -> Vec<Migration> {
    let runs = Runs::new(invocations);
    let results: Vec<Mutex<Option<Migration>>> = sources.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers() {
            // Recorded runs may recurse as deeply as their limits allow.
            let worker = std::thread::Builder::new().stack_size(1 << 30);
            let spawned = worker.spawn_scoped(scope, || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((label, source)) = sources.get(index) else {
                        break;
                    };
                    let repaired =
                        migrate::repair(source, &migrations[index], &runs.of(label), observations);
                    *results[index].lock().unwrap() = Some(repaired);
                }
            });
            spawned.expect("spawn a repair worker");
        }
    });
    results
        .into_iter()
        .map(|slot| slot.into_inner().unwrap().unwrap())
        .collect()
}
