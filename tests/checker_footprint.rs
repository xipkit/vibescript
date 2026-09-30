//! The type checker accounts for its own memory, which a compilation's
//! memory quota bounds. This test counts every allocation while programs
//! check and requires the real peak of the checker, and of the pass over
//! the canonical surface after it, to stay within a factor of what their
//! own accounts report, so a table an account misses fails here. The two
//! are measured apart, so that neither account's margin hides what the
//! other misses. The checker's peak is also compared between each two of
//! its measures with the larger of them, so scratch it takes and frees
//! between them without counting fails too, even when a table it builds
//! later is as large.
//!
//! A second test compiles wide and deep programs under small memory quotas
//! and requires the real heap never to pass the quota by more than a small
//! allowance before the compilation reports it, so memory that any part of
//! compiling allocates before its account sees it fails there.
//!
//! A third test stops each of those compilations partway, with a step or
//! memory quota at a fraction of what it needs, a cancellation at a
//! fraction of its allocations, a cancelled token or a deadline already
//! past, and requires it to allocate at most a few dozen times, and half a
//! megabyte, from the moment any budget first stops it until it returns,
//! and when cancelled partway, at most 65,536 times and 16 MiB from the
//! cancellation, so work that goes on after it should stop fails there,
//! and so does work that never asks whether it should. The tests have a
//! target of their own, since the allocator is global, and run one at a
//! time.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};
use vibescript::{Engine, typing::Observed};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The live bytes when the checker starts and when the surface pass
/// starts, the checker's peak and the peak when the check ends.
static CHECKING: AtomicUsize = AtomicUsize::new(0);
static SURFACING: AtomicUsize = AtomicUsize::new(0);
static CHECKED: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
/// What the checker's account held at its last measure, without scratch
/// it held for a moment.
static LAST: AtomicUsize = AtomicUsize::new(0);
/// The real peak between two measures that most exceeds what the larger
/// of them allows, or that is the most times it when none does, and that
/// account.
static INTERVAL: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// How many allocations the process has made, and how many bytes they
/// took in all.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

fn added(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(live, Relaxed);
    let count = ALLOCATIONS.fetch_add(1, Relaxed);
    ALLOCATED.fetch_add(bytes, Relaxed);
    if count == CANCEL_AT.load(Relaxed) {
        // Cancelling stores a flag and allocates nothing.
        if let Ok(token) = CANCEL.try_lock() {
            if let Some(token) = &*token {
                token.cancel();
                CANCELLED_ALLOCATIONS.store(count, Relaxed);
                CANCELLED_ALLOCATED.store(ALLOCATED.load(Relaxed), Relaxed);
            }
        }
    }
}

// SAFETY: Every operation delegates to System with the original layout and
// pointer. Accounting uses atomics and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies a valid allocation layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            added(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies a valid allocation layout.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            added(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        // SAFETY: The caller supplies the original pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: The caller supplies the original pointer/layout and valid size.
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            LIVE.fetch_sub(layout.size(), Relaxed);
            added(size);
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Held by each test while it runs, since the allocator counts every
/// thread's allocations.
static SERIAL: Mutex<()> = Mutex::new(());

/// How far the real peak may exceed the checker's own account.
const FACTOR: f64 = 1.5;
/// Allocations of every check that no table holds, such as the thread a
/// long source is checked on.
const SLACK: usize = 32 << 10;

/// A pass's real peak heap above what was live when it started, and the
/// peak its account reports; for the checker, also the worst real peak
/// between two of its measures, and the larger of them.
struct Measured {
    checker: (usize, usize),
    surface: (usize, usize),
    interval: (usize, usize),
}

/// Follows a check: marks where each pass starts and ends, and compares
/// the real peak between each two of the checker's measures with them.
fn observe(observed: Observed) {
    match observed {
        Observed::Checking => {
            let live = LIVE.load(Relaxed);
            CHECKING.store(live, Relaxed);
            SURFACING.store(usize::MAX, Relaxed);
            CHECKED.store(live, Relaxed);
            LAST.store(0, Relaxed);
            *INTERVAL.lock().unwrap() = (0, 0);
            PEAK.store(live, Relaxed);
        }
        Observed::Measured {
            held,
            peak: account,
        } => {
            let peak = PEAK.swap(LIVE.load(Relaxed), Relaxed);
            CHECKED.fetch_max(peak, Relaxed);
            let real = peak.saturating_sub(CHECKING.load(Relaxed));
            // What was held before this measure, or what it found now with
            // any scratch beside; what stays is where the next one starts.
            let account = account.max(LAST.swap(held, Relaxed));
            let mut worst = INTERVAL.lock().unwrap();
            if badness(real, account) > badness(worst.0, worst.1) {
                *worst = (real, account);
            }
        }
        Observed::Surfacing => {
            CHECKED.fetch_max(PEAK.load(Relaxed), Relaxed);
            let live = LIVE.load(Relaxed);
            SURFACING.store(live, Relaxed);
            PEAK.store(live, Relaxed);
        }
        Observed::Checked => {
            if SURFACING.load(Relaxed) == usize::MAX {
                CHECKED.fetch_max(PEAK.load(Relaxed), Relaxed);
            }
            DONE.store(PEAK.load(Relaxed), Relaxed);
        }
    }
}

/// What each measured peak is: the checker's, the surface pass's, and the
/// checker's worst between two of its measures.
const PASSES: [&str; 3] = ["checker", "surface pass", "checker between measures"];

/// How bad `real` bytes are for an account of `account`: first by how far
/// they exceed what it allows, and when they don't, by how many times it
/// they are, among peaks larger than the slack.
fn badness(real: usize, account: usize) -> (u8, f64) {
    let excess = real as f64 - FACTOR * account as f64 - SLACK as f64;
    if excess > 0.0 {
        (2, excess)
    } else if real > SLACK {
        (1, real as f64 / account.max(1) as f64)
    } else {
        (0, 0.0)
    }
}

/// The real and accounted peaks of the checker and the surface pass while
/// checking `source`, until the check returns.
fn measure(engine: &Engine, source: &str) -> Measured {
    let checked = engine
        .type_check_with(source, observe)
        .expect("the source parses");
    let checking = CHECKING.load(Relaxed);
    let surfacing = SURFACING.load(Relaxed);
    let checker = CHECKED.load(Relaxed) - checking;
    let surface = if surfacing == usize::MAX {
        0
    } else {
        DONE.load(Relaxed) - surfacing
    };
    Measured {
        checker: (checker, checked.peak_bytes),
        surface: (surface, checked.surface_bytes),
        interval: *INTERVAL.lock().unwrap(),
    }
}

/// Programs whose shapes stress the checker's tables.
fn adversarial() -> Vec<(String, String)> {
    let small = cfg!(target_os = "wasi");
    let scale = |n: usize| if small { n / 8 } else { n };
    let join = |count: usize, item: &dyn Fn(usize) -> String, separator: &str| {
        (0..count).map(item).collect::<Vec<_>>().join(separator)
    };
    let mut programs = Vec::new();
    let arms = join(1_000, &|i| format!("{{a{i}: int}}"), " | ");
    programs.push((
        "a 1,000-arm union of shapes narrowed by an ensure".to_owned(),
        format!(
            "type Wide = {arms}\ndef f(x: Wide?) -> Wide?\n  begin\n    1\n  ensure\n    return nil if x == nil\n  end\n  x\nend\n"
        ),
    ));
    let loose =
        |prefix: &str, extra: &str| join(500, &|i| format!("{{{prefix}{i}?: int{extra}}}"), " | ");
    programs.push((
        "two 500-arm unions of optional-field shapes".to_owned(),
        format!(
            "type A = {}\ntype B = {}\ndef f(x: A, y: B?) -> B?\n  z: A? = x\n  y\nend\n",
            loose("a", ""),
            loose("b", ", x: int")
        ),
    ));
    let fields = scale(16_000);
    programs.push((
        "a 16,000-field shape".to_owned(),
        format!(
            "type Big = {{ {} }}\ndef f(b: Big) -> int\n  c: Big = b\n  c[\"f0\"]\nend\nx: Big = {{ {} }}\np(f(x))\n",
            join(fields, &|i| format!("f{i}: int"), ", "),
            join(fields, &|i| format!("f{i}: {i}"), ", ")
        ),
    ));
    let members = scale(100_000);
    programs.push((
        "a 100,000-member enum".to_owned(),
        format!(
            "enum E\n{}end\ndef f(e: E) -> int\n  case e\n  when E::M0 then 0\n  else 1\n  end\nend\np(f(E::M7))\n",
            join(members, &|i| format!("  M{i}\n"), "")
        ),
    ));
    programs.push((
        "a 100,000-member enum given an unknown symbol and an incomplete case".to_owned(),
        format!(
            "enum E\n{}end\ndef f(e: E) -> int\n  case e\n  when E::M0 then 0\n  end\nend\np(f(:nope))\n",
            join(members, &|i| format!("  M{i}\n"), "")
        ),
    ));
    let variables = scale(20_000);
    programs.push((
        "a class whose initialize leaves 20,000 variables unassigned".to_owned(),
        format!(
            "class C\n{}  def initialize\n  end\nend\n",
            join(variables, &|i| format!("  @v{i}: int\n"), "")
        ),
    ));
    // Each level's fields are the level below, so a mismatch spelling
    // the type out in full would repeat the innermost 8^5 times.
    let nested = (1..6).fold(
        vec![format!(
            "type T0 = {{ {} }}",
            join(8, &|i| format!("x{i}: int"), ", ")
        )],
        |mut lines, depth| {
            lines.push(format!(
                "type T{depth} = {{ {} }}",
                join(8, &|i| format!("f{i}: T{}", depth - 1), ", ")
            ));
            lines
        },
    );
    programs.push((
        "a mismatch with a shape nested six deep through aliases".to_owned(),
        format!("{}\nx: T5 = 1\n", nested.join("\n")),
    ));
    programs.push((
        "a begin holding a 200,000-element array".to_owned(),
        format!(
            "begin\n  x = [{}]\nrescue\n  x = []\nend\n",
            join(scale(200_000), &|_| "1".to_owned(), ", ")
        ),
    ));
    let wide = join(scale(200_000), &|_| "1".to_owned(), ", ");
    programs.push((
        "a module body holding a 200,000-element array".to_owned(),
        format!("module M\n  X = [{wide}]\nend\n"),
    ));
    programs.push((
        "a function holding a 200,000-element array".to_owned(),
        format!("def f -> int\n  [{wide}].length\nend\n"),
    ));
    programs.push((
        "an implicit block holding a 200,000-element array".to_owned(),
        format!("x = [1].map {{ [it, {wide}].length }}\n"),
    ));
    programs.push((
        "a mismatch at a statement holding a 200,000-element array".to_owned(),
        format!("def f -> int\n  x = [{wide}]\nend\n"),
    ));
    programs.push((
        "a module holding 20,000 modules".to_owned(),
        format!(
            "module Outer\n{}end\n",
            join(scale(20_000), &|i| format!("  module M{i}\n  end\n"), "")
        ),
    ));
    programs.push((
        "a 20,000-part destructuring".to_owned(),
        format!("{} = []\n", join(scale(20_000), &|i| format!("a{i}"), ", ")),
    ));
    let long = "n".repeat(scale(256_000));
    programs.push((
        "a 256,000-byte name for a local, a parameter and a method".to_owned(),
        format!(
            "def m{long}(p{long}: int) -> int\n  l{long} = p{long} + 1\n  l{long} + p{long}\nend\nx = m{long}(1)\np(x)\n"
        ),
    ));
    let entry = "w".repeat(scale(256_000));
    programs.push((
        "percent literals of long entries".to_owned(),
        format!("x = %w[{entry} a]\ny = %i[{entry} b]\n"),
    ));
    let levels = if small { 100 } else { 900 };
    programs.push((
        "nested begins around many assignments".to_owned(),
        format!(
            "x: int? = 1\nc = true\n{}{}{}",
            "begin\n".repeat(levels),
            "x = 1\n".repeat(10_000),
            "rescue\nc = false\nensure\nc = true\nend\n".repeat(levels)
        ),
    ));
    let blocks = if small { 60 } else { 300 };
    programs.push((
        "nested blocks around many assignments".to_owned(),
        format!(
            "x: int? = 1\n{}{}{}",
            "[1].each { |q|\n".repeat(blocks),
            "x = 1\n".repeat(10_000),
            "}\n".repeat(blocks)
        ),
    ));
    programs.push((
        "nested begins around many narrowed locals".to_owned(),
        format!(
            "{}{}{}{}",
            join(2_000, &|i| format!("x{i}: int? = 1\n"), ""),
            "begin\n".repeat(if small { 60 } else { 200 }),
            join(2_000, &|i| format!("x{i} = nil\n"), ""),
            "rescue\nc = 1\nensure\nc = 2\nend\n".repeat(if small { 60 } else { 200 })
        ),
    ));
    let values = ["1", "\"a\"", "2.5", "nil", "true", ":s", "[1]"];
    programs.push((
        "a 40,000-element mixed literal".to_owned(),
        format!(
            "x = [{}]\np(x.length)\n",
            join(
                scale(40_000),
                &|i| values[i % values.len()].to_owned(),
                ", "
            )
        ),
    ));
    programs.push((
        "a 40,000-element literal in an interpolation".to_owned(),
        format!(
            "x = \"#{{[{}].length}}\"\np(x)\n",
            join(scale(40_000), &|_| "1".to_owned(), ", ")
        ),
    ));
    programs.push((
        "a 16,000-key hash literal".to_owned(),
        format!(
            "x = {{{}}}\np(x[\"k0\"])\n",
            join(scale(16_000), &|i| format!("k{i}: {i}"), ", ")
        ),
    ));
    // Keys of one to four letters, so 900,000 of them fit a small source.
    let key = |mut i: usize| {
        let mut key = String::new();
        loop {
            key.push((b'a' + (i % 26) as u8) as char);
            i /= 26;
            if i == 0 {
                return key;
            }
        }
    };
    let wide = join(scale(900_000), &|i| format!("{}: 1", key(i)), ",");
    programs.push((
        "a 900,000-key literal typed as a hash".to_owned(),
        format!("x: hash<string, int> = {{{wide}}}\np(x.length)\n"),
    ));
    programs.push((
        "a 900,000-key literal typed as an open shape or a hash".to_owned(),
        format!(
            "type Open = {{ a: int, b: int, ... }}\nx: Open | hash<string, float> = {{{wide}}}\np(x.length)\n"
        ),
    ));
    programs.push((
        "5,000 functions".to_owned(),
        join(
            scale(5_000),
            &|i| format!("def f{i}(n: int) -> int\n  n + {i}\nend\n"),
            "",
        ),
    ));
    programs.push((
        "1,000 classes".to_owned(),
        join(
            scale(1_000),
            &|i| format!("class C{i}\n  @v: int = {i}\n  def get -> int\n    @v\n  end\nend\n"),
            "",
        ),
    ));
    programs.push((
        "500 calls on a 500-class union".to_owned(),
        format!(
            "{}\ntype U = {}\ndef f(x: U) -> int\n  t = 0\n{}  t\nend\n",
            join(
                500,
                &|i| format!("class C{i}\n  def m -> int\n    {i}\n  end\nend"),
                "\n"
            ),
            join(500, &|i| format!("C{i}"), " | "),
            "  t += x.m\n".repeat(500)
        ),
    ));
    programs.push((
        "modules nested under many locals".to_owned(),
        format!(
            "{}{}X = 1\n{}",
            join(5_000, &|i| format!("y{i} = 1\n"), ""),
            join(
                if small { 60 } else { 500 },
                &|i| format!("module M{i}\n"),
                ""
            ),
            "end\n".repeat(if small { 60 } else { 500 })
        ),
    ));
    let depth = if small { 60 } else { 500 };
    programs.push((
        "classes nested 500 deep".to_owned(),
        format!(
            "{}X = 1\n{}",
            join(depth, &|i| format!("class C{i}\n  @v{i}: int = {i}\n"), ""),
            "end\n".repeat(depth)
        ),
    ));
    // Each index on the union's first alternative records the types under
    // it and sets aside the record of the index around it.
    let (nesting, width) = if small { (20, 500) } else { (100, 2_000) };
    let literal = format!("[{}].length", vec!["1"; width].join(", "));
    let mut index = "0".to_owned();
    for _ in 0..nesting {
        index = format!("u[{literal} + g({index})]");
    }
    programs.push((
        "union indexes nested 100 deep".to_owned(),
        format!(
            "def g(v: int | float | nil) -> int\n  0\nend\ndef f(u: array<int> | array<float>) -> int\n  x = {index}\n  0\nend\n"
        ),
    ));
    // Each call on `self` while variables are unassigned keeps the set of
    // those still unassigned, which the next assignment copies a path of.
    let (classes, variables) = if small { (4, 500) } else { (20, 2_000) };
    programs.push((
        "classes whose initialize calls a method between assignments".to_owned(),
        join(
            classes,
            &|class| {
                format!(
                    "class C{class}\n{}  def initialize\n{}  end\n  def touch -> int\n    1\n  end\nend\n",
                    join(variables, &|i| format!("  @v{i}: int\n"), ""),
                    join(variables, &|i| format!("    @v{i} = 1\n    touch\n"), "")
                )
            },
            "",
        ),
    ));
    programs.push((
        "diagnostics for many wrong calls".to_owned(),
        format!(
            "type Wide = {arms}\ndef f(x: Wide) -> int\n  1\nend\n{}",
            "f(1)\n".repeat(scale(2_000))
        ),
    ));
    programs
}

/// A required file whose body calls its functions many times after
/// assigning many top-level locals, which the functions read through a
/// chain of calls, so each call records the locals assigned before it.
fn required_file() -> String {
    let (locals, chain, calls) = if cfg!(target_os = "wasi") {
        (100, 50, 100)
    } else {
        (400, 200, 400)
    };
    let mut source: String = (0..locals).map(|i| format!("x{i} = {i}\n")).collect();
    for i in 0..chain {
        let next = if i + 1 < chain {
            format!(" + g{}()", i + 1)
        } else {
            String::new()
        };
        source.push_str(&format!("def g{i} -> int\n  x{i}{next}\nend\n"));
    }
    source.push_str(&"g0()\n".repeat(calls));
    source
}

/// Every site and upstream program, and a sample of the language corpus.
fn corpora() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut programs = Vec::new();
    let mut pending = vec![root.join("tests/site"), root.join("tests/upstream")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "vibe")
            {
                programs.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let corpus = std::fs::read_to_string(root.join("tests/language.json")).unwrap();
    let cases: serde_json::Value = serde_json::from_str(&corpus).unwrap();
    let every = if cfg!(target_os = "wasi") { 2_000 } else { 100 };
    for case in cases.as_array().unwrap().iter().step_by(every) {
        if let (Some(name), Some(source)) = (case["name"].as_str(), case["source"].as_str()) {
            programs.push((name.to_owned(), source.to_owned()));
        }
    }
    programs
}

#[test]
fn the_checkers_account_covers_its_peak_memory() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let engine = Engine::new();
    // The builtin signature tables load once, outside any check.
    measure(&engine, "def f(x: array<int>) -> int\n  x.length\nend\n");
    let mut requiring = Engine::new();
    requiring
        .set_module_sources(BTreeMap::from([("big.vibe".to_owned(), required_file())]))
        .unwrap();
    let required = (
        &requiring,
        "a required file's calls and chained reads".to_owned(),
        "require(\"big\")\np(1)\n".to_owned(),
    );
    let log = std::env::var_os("VIBES_FOOTPRINT").is_some();
    let mut failures = Vec::new();
    let mut ratios = [
        (0.0, String::new()),
        (0.0, String::new()),
        (0.0, String::new()),
    ];
    let mut least = [
        (f64::MAX, String::new()),
        (f64::MAX, String::new()),
        (f64::MAX, String::new()),
    ];
    let programs = adversarial().into_iter().chain(corpora());
    for (engine, name, source) in programs
        .map(|(name, source)| (&engine, name, source))
        .chain([required])
    {
        let measured = measure(engine, &source);
        let passes = [measured.checker, measured.surface, measured.interval];
        for (pass, (real, estimate)) in passes.into_iter().enumerate() {
            let ratio = real as f64 / estimate.max(1) as f64;
            if log {
                println!("{pass} {ratio:.2} {real} {estimate} {name}");
            }
            if real > SLACK {
                if ratio > ratios[pass].0 {
                    ratios[pass] = (ratio, name.clone());
                }
                if ratio < least[pass].0 {
                    least[pass] = (ratio, name.clone());
                }
            }
            if real as f64 > FACTOR * estimate as f64 + SLACK as f64 {
                let pass = PASSES[pass];
                failures.push(format!(
                    "{name}: the {pass} held {real} bytes, {estimate} accounted ({ratio:.2}x)"
                ));
            }
        }
    }
    for (pass, name) in PASSES.into_iter().enumerate() {
        println!(
            "the {name}'s real peak is {:.2} to {:.2} times its account, in {} and {}",
            least[pass].0, ratios[pass].0, least[pass].1, ratios[pass].1
        );
    }
    assert!(
        failures.is_empty(),
        "{} passes outgrew their account:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// How much the real heap of a metered compilation may pass its memory
/// quota, beyond an eighth of the quota, before the compilation reports
/// the quota. The accounts estimate hash tables and other capacities, which
/// the eighth allows for; this covers what they bound without counting it
/// exactly, such as the stack a syntax tree's teardown keeps.
const OVERSHOOT: usize = 256 << 10;

/// A family of programs that grow with `n`: a script and the files it
/// requires.
type Shape = (&'static str, fn(usize) -> (Vec<(String, String)>, String));

fn lines(n: usize, line: impl Fn(usize) -> String) -> String {
    (0..n).map(line).collect()
}

fn listed(n: usize, item: impl Fn(usize) -> String, separator: &str) -> String {
    (0..n).map(item).collect::<Vec<_>>().join(separator)
}

/// Programs that grow wide or deep: wide enums, shapes and unions, many
/// interpolations, many locals of one wide type, nested and deep syntax,
/// required files, construction checks and diagnostics.
fn shapes() -> Vec<Shape> {
    vec![
        ("a wide enum", |n| {
            let members = lines(n, |i| format!("  M{i}\n"));
            (Vec::new(), format!("enum E\n{members}end\np(E::M0)\n"))
        }),
        ("a wide enum of long names", |n| {
            let pad = "Ab".repeat(100);
            let members = lines(n / 10 + 1, |i| format!("  M{pad}{i}\n"));
            (Vec::new(), format!("enum E\n{members}end\np(1)\n"))
        }),
        (
            "a wide enum given an unknown symbol and an incomplete case",
            |n| {
                let members = lines(n, |i| format!("  M{i}\n"));
                (
                    Vec::new(),
                    format!(
                        "enum E\n{members}end\ndef f(e: E) -> int\n  case e\n  when E::M0 then 0\n  end\nend\np(f(:nope))\n"
                    ),
                )
            },
        ),
        ("many interpolations", |n| {
            let literal = listed(200, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                lines(n, |i| format!("x{i} = \"#{{[{literal}].length}}\"\n")),
            )
        }),
        ("many tokens after one interpolation", |n| {
            let rest = lines(n, |i| format!("y{i} = [{i}, {i}, {i}]\n"));
            (Vec::new(), format!("s = \"#{{1}}\"\n{rest}"))
        }),
        ("one large interpolation", |n| {
            let literal = listed(n, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("x = \"#{{[{literal}].length}}\"\np(x)\n"),
            )
        }),
        ("many locals sharing a wide union", |n| {
            let arms = listed(700, |i| format!("{{a{i}: int}}"), " | ");
            let locals = lines(n, |i| format!("x{i} = f\n"));
            (
                Vec::new(),
                format!("type U = {arms}\ndef f -> U\n  {{a0: 1}}\nend\n{locals}"),
            )
        }),
        ("a wide shape", |n| {
            let fields = listed(n, |i| format!("f{i}: int"), ", ");
            let values = listed(n, |i| format!("f{i}: {i}"), ", ");
            (
                Vec::new(),
                format!("type S = {{ {fields} }}\nx: S = {{ {values} }}\np(x)\n"),
            )
        }),
        ("a wide union", |n| {
            let arms = listed(n.min(1_000), |i| format!("{{a{i}: int}}"), " | ");
            (
                Vec::new(),
                format!("type U = {arms}\ndef f(x: U?) -> U?\n  x\nend\np(f(nil))\n"),
            )
        }),
        ("many wrong calls with a wide type", |n| {
            let arms = listed(1_000, |i| format!("{{a{i}: int}}"), " | ");
            let calls = "f(1)\n".repeat(n);
            (
                Vec::new(),
                format!("type Wide = {arms}\ndef f(x: Wide) -> int\n  1\nend\n{calls}"),
            )
        }),
        ("nested begins around many narrowed locals", |n| {
            let levels = n.min(400);
            (
                Vec::new(),
                format!(
                    "{}{}{}{}",
                    lines(n, |i| format!("x{i}: int? = 1\n")),
                    "begin\n".repeat(levels),
                    lines(n, |i| format!("x{i} = nil\n")),
                    "rescue\nc = 1\nensure\nc = 2\nend\n".repeat(levels)
                ),
            )
        }),
        ("deep begins", |n| {
            let depth = n.min(1_000);
            let (open, close) = (
                "begin\n".repeat(depth),
                "rescue\nc = 2\nend\n".repeat(depth),
            );
            (Vec::new(), format!("c = 0\n{open}c = 1\n{close}p(c)\n"))
        }),
        ("deep ifs", |n| {
            let depth = n.min(1_000);
            let (open, close) = ("if c == 0\n".repeat(depth), "end\n".repeat(depth));
            (Vec::new(), format!("c = 0\n{open}c = 1\n{close}p(c)\n"))
        }),
        ("deep blocks", |n| {
            let depth = n.min(400);
            let (open, close) = ("[1].each { |q|\n".repeat(depth), "}\n".repeat(depth));
            (Vec::new(), format!("c = 0\n{open}c = 1\n{close}p(c)\n"))
        }),
        ("deep arrays", |n| {
            let depth = n.min(1_000);
            (
                Vec::new(),
                format!("x = {}1{}\np(x)\n", "[".repeat(depth), "]".repeat(depth)),
            )
        }),
        ("nested union indexes", |n| {
            let wide = format!("[{}].length", listed(200, |_| "1".to_owned(), ", "));
            let mut index = "0".to_owned();
            for _ in 0..n.min(150) {
                index = format!("u[{wide} + g({index})]");
            }
            (
                Vec::new(),
                format!(
                    "def g(v: int | float | nil) -> int\n  0\nend\ndef f(u: array<int> | array<float>) -> int\n  x = {index}\n  0\nend\n"
                ),
            )
        }),
        ("a required file", |n| {
            let file = lines(n, |i| format!("x{i} = [{i}, {i}].length\n"));
            (
                vec![("big.vibe".to_owned(), file)],
                "require(\"big\")\np(1)\n".to_owned(),
            )
        }),
        ("construction checks", |n| {
            let class = |class: usize| {
                format!(
                    "class C{class}\n{}  def initialize\n{}  end\n  def touch -> int\n    1\n  end\nend\n",
                    lines(1_000, |i| format!("  @v{i}: int\n")),
                    lines(1_000, |i| format!("    @v{i} = 1\n    touch\n"))
                )
            };
            (Vec::new(), lines((n / 1_000).max(1), class))
        }),
        ("a class leaving variables unassigned", |n| {
            let variables = lines(n, |i| format!("  @v{i}: int\n"));
            (
                Vec::new(),
                format!("class C\n{variables}  def initialize\n  end\nend\n"),
            )
        }),
        ("a long symbol", |n| {
            let name = "s".repeat(n * 64);
            (Vec::new(), format!("x = :{name}\np(x)\n"))
        }),
        ("long percent-literal entries", |n| {
            let word = "w".repeat(n * 64);
            (
                Vec::new(),
                format!("x = %w[{word} a]\ny = %i[{word} b]\np(x)\n"),
            )
        }),
        ("a begin holding a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("begin\n  x = [{items}]\nrescue\n  x = []\nend\np(x)\n"),
            )
        }),
        ("a module body holding a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("module M\n  X = [{items}]\nend\np(M::X.length)\n"),
            )
        }),
        ("a function holding a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("def f -> int\n  [{items}].length\nend\np(f)\n"),
            )
        }),
        ("an implicit block holding a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("x = [1].map {{ [it, {items}].length }}\np(x)\n"),
            )
        }),
        ("a module holding many modules", |n| {
            let modules = lines(n, |i| format!("  module M{i}\n  end\n"));
            (Vec::new(), format!("module Outer\n{modules}end\np(1)\n"))
        }),
        ("a wide destructuring", |n| {
            let targets = listed(n, |i| format!("a{i}"), ", ");
            (Vec::new(), format!("{targets} = []\np(a0)\n"))
        }),
        ("a mismatch at a statement holding a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (
                Vec::new(),
                format!("def f -> int\n  x = [{items}]\nend\np(1)\n"),
            )
        }),
        ("a compound assignment of a wide array", |n| {
            let items = listed(n * 4, |_| "1".to_owned(), ", ");
            (Vec::new(), format!("x = [1]\nx += [{items}]\np(x)\n"))
        }),
        ("a long name for a local, a parameter and a method", |n| {
            let long = "n".repeat(n * 64);
            (
                Vec::new(),
                format!(
                    "def m{long}(p{long}: int) -> int\n  l{long} = p{long} + 1\n  l{long} + p{long}\nend\nx = m{long}(1)\np(x)\n"
                ),
            )
        }),
        (
            "a required file of many public functions and methods",
            |n| {
                // Long names, which a signature and its export each copy.
                let long = "e".repeat(256);
                let functions = lines(n / 8 + 1, |i| {
                    format!("def f{i}{long}(a: int) -> int\n  a\nend\n")
                });
                let methods = lines(n / 8 + 1, |i| {
                    format!("  def m{i}{long}(a: int) -> int\n    a\n  end\n")
                });
                (
                    vec![(
                        "big.vibe".to_owned(),
                        format!("{functions}class C\n{methods}end\n"),
                    )],
                    "require(\"big\")\np(1)\n".to_owned(),
                )
            },
        ),
        ("a very wide interpolation", |n| {
            let literal = listed(n * 4, |_| "1".to_owned(), ",");
            (Vec::new(), format!("x = \"#{{[{literal}]}}\"\np(x)\n"))
        }),
        ("many functions each around nested begins", |n| {
            // Each level's handlers list the locals of every level inside
            // it, which the program keeps for each function.
            let levels = if cfg!(target_os = "wasi") { 20 } else { 40 };
            let body = format!(
                "{}{}{}",
                "  begin\n".repeat(levels),
                lines(levels, |i| format!("  x{i} = {i}\n")),
                "  rescue\n  c = 1\n  end\n".repeat(levels)
            );
            let functions = lines(n / 40 + 1, |i| format!("def f{i} -> int\n{body}  0\nend\n"));
            (Vec::new(), format!("{functions}p(1)\n"))
        }),
        ("a wide keyword call", |n| {
            // An overloaded method, which lists the call's keywords to choose
            // among its signatures.
            let keywords = listed(n, |i| format!("k{i}: {i}"), ", ");
            (Vec::new(), format!("x = [1, 2].first({keywords})\np(x)\n"))
        }),
        ("many removed spellings", |n| {
            // Each a rewrite the pass over the canonical surface reports.
            (Vec::new(), lines(n, |i| format!("x{i} = %w[a b]\n")))
        }),
        ("many classes with one default each", |n| {
            let classes = lines(n, |i| format!("class C{i}\n  @v: int = {i}\nend\n"));
            (Vec::new(), format!("{classes}p(1)\n"))
        }),
        ("many empty functions and classes", |n| {
            let functions = lines(n, |i| format!("def f{i}\nend\n"));
            let classes = lines(n, |i| format!("class C{i}\nend\n"));
            (Vec::new(), format!("{functions}{classes}p(1)\n"))
        }),
        ("a large required file that runs out mid-check", |n| {
            let functions = lines(n, |i| {
                format!("def f{i}(a: int, b: string, c: array<int>) -> int\n  a\nend\n")
            });
            let body = lines(n, |i| format!("x{i} = f{i}(1, \"b\", [1])\n"));
            (
                vec![("big.vibe".to_owned(), format!("{functions}{body}"))],
                "require(\"big\")\np(1)\n".to_owned(),
            )
        }),
        ("a mismatch with shapes nested through aliases", |n| {
            let depth = (n / 1_000).clamp(1, 8);
            let mut source = format!(
                "type T0 = {{ {} }}\n",
                listed(8, |i| format!("x{i}: int"), ", ")
            );
            for level in 1..=depth {
                let fields = listed(8, |i| format!("f{i}: T{}", level - 1), ", ");
                source.push_str(&format!("type T{level} = {{ {fields} }}\n"));
            }
            source.push_str(&format!("x: T{depth} = 1\n"));
            (Vec::new(), source)
        }),
        ("a required file with a syntax error first", |n| {
            // Its first line fails the metered parse at once, and the rest
            // is parsed again to report every error the file has.
            let rest = lines(n, |i| format!("y{i} = [{i}, {i}]\n"));
            (
                vec![("big.vibe".to_owned(), format!("x = )\n{rest}"))],
                "require(\"big\")\np(1)\n".to_owned(),
            )
        }),
        ("type errors among removed spellings", |n| {
            // Each spelling's diagnostic replaces the checker's inside it,
            // which merging the two lists finds.
            (
                Vec::new(),
                lines(n, |i| format!("x{i}: string = 1\nn{i} = [1].size\n")),
            )
        }),
        ("a wide self-call graph", |n| {
            // Each method calls itself, a cycle of one for each of them in
            // the graph of the calls construction checks follow.
            let methods = lines(n, |i| format!("  def m{i} -> int\n    m{i}\n  end\n"));
            (Vec::new(), format!("class C\n{methods}end\np(1)\n"))
        }),
        // Suffixed bindings fail with V0003, whose fix pass parses the
        // source again to find every place the fix renames.
        ("many suffixed locals, each read", |n| {
            (Vec::new(), lines(n, |i| format!("x{i}? = {i}\np(x{i}?)\n")))
        }),
        ("a suffixed local read many times", |n| {
            (Vec::new(), format!("ok? = 1\n{}", "p(ok?)\n".repeat(n)))
        }),
        ("suffixed constants read through their modules", |n| {
            let modules = lines(n, |i| {
                format!("module M{i}\n  MAX! = {i}\nend\np(M{i}::MAX!)\n")
            });
            (Vec::new(), modules)
        }),
        ("a suffixed constant read through a deep path", |n| {
            let depth = 40;
            let open = lines(depth, |i| format!("module A{i}\n"));
            let close = "end\n".repeat(depth);
            let path = listed(depth, |i| format!("A{i}"), "::");
            let reads = lines(n, |_| format!("p({path}::MAX!)\n"));
            (Vec::new(), format!("{open}MAX! = 1\n{close}{reads}"))
        }),
        ("suffixed parameters of many functions", |n| {
            let functions = lines(n, |i| {
                format!("def f{i}(list!: array<int>) -> int\n  list!.size\nend\n")
            });
            (Vec::new(), functions)
        }),
        ("many suffixed classes, each constructed", |n| {
            let classes = lines(n, |i| format!("class C{i}?\nend\nc{i} = C{i}?.new\n"));
            (Vec::new(), classes)
        }),
        ("suffixed locals whose fixes collide", |n| {
            (
                Vec::new(),
                lines(n, |i| format!("x{i}? = {i}\nx{i} = {i}\n")),
            )
        }),
    ]
}

/// An engine that requires `modules`, or `None` when it refuses them.
fn engine_with(modules: Vec<(String, String)>) -> Option<Engine> {
    let mut engine = Engine::new();
    if !modules.is_empty() {
        engine
            .set_module_sources(modules.into_iter().collect())
            .ok()?;
    }
    Some(engine)
}

/// The real peak heap of compiling `source` under `options`, above what
/// was live when it started, and what the compilation gave.
fn compiled_peak(
    engine: &Engine,
    source: &str,
    options: &vibescript::CallOptions,
) -> (usize, Result<(), vibescript::ErrorKind>) {
    let live = LIVE.load(Relaxed);
    PEAK.store(live, Relaxed);
    let result = engine
        .compile_with_options(source, options)
        .map(drop)
        .map_err(|error| error.kind);
    (PEAK.load(Relaxed) - live, result)
}

#[test]
fn a_metered_compilation_stays_within_its_memory_quota() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let small = cfg!(target_os = "wasi");
    let quotas: &[usize] = if small {
        &[1 << 20, 3 << 20]
    } else {
        &[1 << 20, 3 << 20, 4 << 20, 16 << 20]
    };
    let unlimited = vibescript::CallOptions {
        limits: vibescript::Limits {
            steps: None,
            memory_bytes: None,
            ..vibescript::Limits::default()
        },
        ..vibescript::CallOptions::default()
    };
    let mut failures = Vec::new();
    for (name, shape) in shapes() {
        // What the process loads once, such as builtin tables, loads before
        // any compilation is measured.
        let (modules, source) = shape(500);
        if let Some(engine) = engine_with(modules) {
            let _ = engine.compile_with_options(&source, &unlimited);
        }
        for &quota in quotas {
            let options = vibescript::CallOptions {
                limits: vibescript::Limits {
                    steps: None,
                    memory_bytes: Some(quota),
                    ..vibescript::Limits::default()
                },
                ..vibescript::CallOptions::default()
            };
            let mut n = 500;
            loop {
                let (modules, source) = shape(n);
                let size = source.len() + modules.iter().map(|(_, file)| file.len()).sum::<usize>();
                if size > quota || n > 1 << 20 {
                    break;
                }
                let Some(engine) = engine_with(modules) else {
                    break;
                };
                let (peak, result) = compiled_peak(&engine, &source, &options);
                let allowed = quota + quota / 8 + OVERSHOOT;
                if peak > allowed {
                    failures.push(format!(
                        "{name} of {n} under {} KiB: {peak} bytes at the peak, {allowed} allowed ({result:?})",
                        quota >> 10
                    ));
                }
                n *= 2;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} compilations passed their quota:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_sources_required_files_keep_count_toward_the_quota() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A host's files, each small enough to check within the quota and each
    // with a type error, whose diagnostic keeps the file's source after its
    // check: many more of them than the quota holds.
    let (files, quota) = if cfg!(any(debug_assertions, target_os = "wasi")) {
        (250, 4 << 20)
    } else {
        (1_000, 16 << 20)
    };
    let body = lines(2_000, |i| format!("x{i} = {i}\n"));
    let modules = (0..files)
        .map(|k| (format!("m{k}.vibe"), format!("{body}y: string = 1\n")))
        .collect();
    let mut source: String = (0..files).map(|k| format!("require(\"m{k}\")\n")).collect();
    source.push_str("p(1)\n");
    let engine = engine_with(modules).unwrap();
    // What the process loads once loads before the compilation is
    // measured.
    let _ = engine.compile_with_options("p(1)\n", &limited(None, None));
    let (peak, result) = compiled_peak(&engine, &source, &limited(None, Some(quota)));
    let allowed = quota + quota / 8 + OVERSHOOT;
    assert!(
        peak <= allowed,
        "{peak} bytes at the peak, {allowed} allowed ({result:?})"
    );
}

/// [`ALLOCATIONS`] and [`ALLOCATED`] when a budget first stopped the
/// compilation being measured, or `usize::MAX` before one does.
static TRIPPED_ALLOCATIONS: AtomicUsize = AtomicUsize::new(usize::MAX);
static TRIPPED_ALLOCATED: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The allocation at which the allocator cancels [`CANCEL`], so that a
/// compilation is cancelled partway through, or `usize::MAX` for none.
static CANCEL_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
static CANCEL: Mutex<Option<vibescript::CancellationToken>> = Mutex::new(None);
/// [`ALLOCATIONS`] and [`ALLOCATED`] when the allocator cancelled
/// [`CANCEL`].
static CANCELLED_ALLOCATIONS: AtomicUsize = AtomicUsize::new(usize::MAX);
static CANCELLED_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

/// The most allocations, and bytes, a compilation may make after any of
/// its budgets first stops it, before it returns: unwinding checks nothing
/// more, and frees what it built, which takes at most a stack as deep as
/// the syntax to tear down.
const AFTER_TRIP_ALLOCATIONS: usize = 64;
const AFTER_TRIP_BYTES: usize = 512 << 10;

/// The most allocations, and bytes, a compilation may make after it is
/// cancelled, before it returns: it notices within a few polls of the
/// checker, a few thousand steps of its work or a checkpoint of the parser,
/// and then stops as promptly as it does for any budget.
const AFTER_CANCEL_ALLOCATIONS: usize = 1 << 16;
const AFTER_CANCEL_BYTES: usize = 16 << 20;

/// Marks the first moment a budget stops the work.
fn tripped() {
    if TRIPPED_ALLOCATIONS
        .compare_exchange(usize::MAX, ALLOCATIONS.load(Relaxed), Relaxed, Relaxed)
        .is_ok()
    {
        TRIPPED_ALLOCATED.store(ALLOCATED.load(Relaxed), Relaxed);
    }
}

/// What compiling `source` under `options` allocated after a budget first
/// stopped it, in allocations and bytes, or `None` when none did, and what
/// the compilation gave.
fn after_trip(
    engine: &Engine,
    source: &str,
    options: &vibescript::CallOptions,
) -> (Option<(usize, usize)>, Result<(), vibescript::ErrorKind>) {
    TRIPPED_ALLOCATIONS.store(usize::MAX, Relaxed);
    vibescript::set_budget_hook(Some(tripped));
    let result = engine
        .compile_with_options(source, options)
        .map(drop)
        .map_err(|error| error.kind);
    vibescript::set_budget_hook(None);
    let at = TRIPPED_ALLOCATIONS.load(Relaxed);
    let after = (at != usize::MAX).then(|| {
        (
            ALLOCATIONS.load(Relaxed) - at,
            ALLOCATED.load(Relaxed) - TRIPPED_ALLOCATED.load(Relaxed),
        )
    });
    (after, result)
}

/// Options with a step quota, a memory quota, or neither.
fn limited(steps: Option<u64>, memory: Option<usize>) -> vibescript::CallOptions {
    vibescript::CallOptions {
        limits: vibescript::Limits {
            steps,
            memory_bytes: memory,
            ..vibescript::Limits::default()
        },
        ..vibescript::CallOptions::default()
    }
}

#[test]
fn a_stopped_compilation_stops_promptly() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Unoptimized builds, and WASI, check smaller programs, in time.
    let n = if cfg!(any(debug_assertions, target_os = "wasi")) {
        1_000
    } else {
        4_000
    };
    let mut failures = Vec::new();
    let mut trips = 0;
    for (name, shape) in shapes() {
        let (modules, source) = shape(n);
        let Some(engine) = engine_with(modules) else {
            continue;
        };
        // What the process loads once loads before any compilation is
        // measured; this one also gives the compilation's allocations and
        // real peak.
        let _ = engine.compile_with_options(&source, &limited(None, None));
        let before = ALLOCATIONS.load(Relaxed);
        let (peak, _) = compiled_peak(&engine, &source, &limited(None, None));
        let allocations = ALLOCATIONS.load(Relaxed) - before;
        // Its cost in steps, to within a factor of four.
        let mut cost = 1u64 << 12;
        while cost < 1 << 40 {
            let (_, result) = after_trip(&engine, &source, &limited(Some(cost), None));
            if result != Err(vibescript::ErrorKind::Steps) {
                break;
            }
            cost *= 4;
        }
        let mut budgets: Vec<(String, vibescript::CallOptions, Option<usize>)> = Vec::new();
        for divisor in [2u64, 4, 8, 16, 32, 64, 256] {
            let quota = cost / divisor;
            budgets.push((format!("{quota} steps"), limited(Some(quota), None), None));
        }
        for divisor in [2usize, 4, 16, 64] {
            let quota = peak / divisor;
            budgets.push((format!("{quota} bytes"), limited(None, Some(quota)), None));
        }
        let cancelled = vibescript::CancellationToken::new();
        cancelled.cancel();
        budgets.push((
            "a cancelled token".to_owned(),
            vibescript::CallOptions {
                cancellation: cancelled,
                ..limited(None, None)
            },
            None,
        ));
        budgets.push((
            "an expired deadline".to_owned(),
            vibescript::CallOptions {
                deadline: Some(std::time::Instant::now()),
                ..limited(None, None)
            },
            None,
        ));
        // Cancellations early on, and at each eighth of the way, in every
        // stage of compiling.
        let eighths = (1..8).map(|eighths| allocations * eighths / 8);
        for at in [allocations / 64, allocations / 16]
            .into_iter()
            .chain(eighths)
        {
            budgets.push((
                format!("a cancellation at allocation {at}"),
                limited(None, None),
                Some(at),
            ));
        }
        for (budget, mut options, cancel_at) in budgets {
            if let Some(at) = cancel_at {
                let token = vibescript::CancellationToken::new();
                options.cancellation = token.clone();
                *CANCEL.lock().unwrap() = Some(token);
                CANCEL_AT.store(ALLOCATIONS.load(Relaxed) + at, Relaxed);
            }
            CANCELLED_ALLOCATIONS.store(usize::MAX, Relaxed);
            let (after, result) = after_trip(&engine, &source, &options);
            let cancelled = CANCELLED_ALLOCATIONS.load(Relaxed);
            if cancel_at.is_some() && cancelled != usize::MAX {
                let allocations = ALLOCATIONS.load(Relaxed) - cancelled;
                let bytes = ALLOCATED.load(Relaxed) - CANCELLED_ALLOCATED.load(Relaxed);
                if allocations > AFTER_CANCEL_ALLOCATIONS || bytes > AFTER_CANCEL_BYTES {
                    failures.push(format!(
                        "{name} under {budget}: {allocations} allocations of {bytes} bytes after it was cancelled ({result:?})"
                    ));
                }
            }
            CANCEL_AT.store(usize::MAX, Relaxed);
            *CANCEL.lock().unwrap() = None;
            let Some((allocations, bytes)) = after else {
                continue;
            };
            trips += 1;
            if allocations > AFTER_TRIP_ALLOCATIONS || bytes > AFTER_TRIP_BYTES {
                failures.push(format!(
                    "{name} under {budget}: {allocations} allocations of {bytes} bytes after it stopped ({result:?})"
                ));
            }
        }
    }
    // Most budgets stop the compilation they bound.
    assert!(trips > shapes().len() * 8, "only {trips} budgets stopped");
    assert!(
        failures.is_empty(),
        "{} compilations went on after they stopped:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
