//! The type checker accounts for its own memory, which a compilation's
//! memory quota bounds. This test counts every allocation while programs
//! check and requires the real peak of the checker, and of the pass over
//! the canonical surface after it, to stay within a factor of what their
//! own accounts report, so a table an account misses fails here. The two
//! are measured apart, so that neither account's margin hides what the
//! other misses. The checker's peak is also compared between each two of
//! its measures with the larger of them, so scratch it takes and frees
//! between them without counting fails too, even when a table it builds
//! later is as large. It has a target of its own, since the allocator is
//! global.

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

fn added(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(live, Relaxed);
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
