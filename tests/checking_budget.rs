use vibescript::{CallOptions, CheckDiagnostic, CheckReport, Engine, ErrorKind, Limits, Script};

const DEFAULT_STEPS: u64 = 1_000_000;

fn unlimited() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: None,
            memory_bytes: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

fn entries(list: &[CheckDiagnostic]) -> Vec<(String, usize, String)> {
    list.iter()
        .map(|entry| (entry.function.clone(), entry.offset, entry.message.clone()))
        .collect()
}

fn same_report(actual: &CheckReport, expected: &CheckReport) {
    assert_eq!(entries(&actual.diagnostics), entries(&expected.diagnostics));
    assert_eq!(entries(&actual.incomplete), entries(&expected.incomplete));
}

// Each program once exhausted the default step quota while checking, through quadratic union
// joins, a caller analysis per new callee specialization, repeated operator and collection
// analysis on every loop walk, or quadratic block exit bookkeeping.
const PROGRAMS: [(&str, &str); 9] = [
    (
        "range_extraction",
        include_str!("site/rosettacode/popular/range_extraction.vibe"),
    ),
    (
        "zig_zag_matrix",
        include_str!("site/rosettacode/popular/zig_zag_matrix.vibe"),
    ),
    (
        "topological_sort",
        include_str!("site/rosettacode/popular/topological_sort.vibe"),
    ),
    (
        "natural_sorting",
        include_str!("site/rosettacode/popular/natural_sorting.vibe"),
    ),
    (
        "heronian_triangles",
        include_str!("site/rosettacode/popular/heronian_triangles.vibe"),
    ),
    (
        "roman_numerals_decode",
        include_str!("site/rosettacode/popular/roman_numerals_decode.vibe"),
    ),
    (
        "spiral_matrix",
        include_str!("site/rosettacode/popular/spiral_matrix.vibe"),
    ),
    (
        "magic_squares_of_odd_order",
        include_str!("site/rosettacode/popular/magic_squares_of_odd_order.vibe"),
    ),
    (
        "chudnovsky_pi",
        include_str!("site/showcase/math/chudnovsky_pi.vibe"),
    ),
];

#[test]
fn loop_heavy_programs_check_within_the_default_budget_with_unchanged_reports() {
    for (name, source) in PROGRAMS {
        let script = Engine::new().compile(source).unwrap();
        let expected = script.check(&unlimited()).unwrap();
        assert!(
            expected.stats.steps < DEFAULT_STEPS,
            "{name}: {:?}",
            expected.stats
        );
        let report = script.check(&CallOptions::default()).unwrap();
        same_report(&report, &expected);
        assert_eq!(report.stats.steps, expected.stats.steps, "{name}");
    }
}

fn sequential_calls(count: usize) -> String {
    let mut source = String::new();
    for index in 0..count {
        source.push_str(&format!("def f{index}\n  {index}\nend\n"));
    }
    source.push_str("def run\n  total = 0\n");
    for index in 0..count {
        source.push_str(&format!("  total = total + f{index}\n"));
    }
    source.push_str("  total\nend\n");
    source
}

fn nested_loops(depth: usize) -> String {
    let mut source = String::from("def g0(n)\n  n + 1\nend\n");
    for level in 1..depth {
        source.push_str(&format!(
            "def g{level}(n)\n  i = 0\n  total = 0\n  while i < n\n    \
             total = total + g{}(i)\n    i = i + 1\n  end\n  total\nend\n",
            level - 1
        ));
    }
    source.push_str(&format!("def run\n  g{}(3)\nend\n", depth - 1));
    source
}

#[test]
fn new_callee_specializations_do_not_repeat_their_callers() {
    // Every call provisionally returned nothing, so each caller pass reached one more callee.
    let script = Engine::new().compile(&sequential_calls(250)).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.stats.steps < DEFAULT_STEPS / 4, "{:?}", report.stats);
    // Callees created inside loops are summarized before the loop continues, beyond the
    // bounded nesting depth as well.
    for depth in [3, 12] {
        let script = Engine::new().compile(&nested_loops(depth)).unwrap();
        let expected = script.check(&unlimited()).unwrap();
        assert!(expected.incomplete.is_empty(), "{expected:?}");
        assert!(
            expected.stats.steps < DEFAULT_STEPS / 3,
            "{:?}",
            expected.stats
        );
        let report = script.check(&CallOptions::default()).unwrap();
        same_report(&report, &expected);
        assert!(script.call("run", &[], CallOptions::default()).is_ok());
    }
}

fn limited(steps: u64, memory: usize) -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(steps),
            memory_bytes: Some(memory),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

fn exact_limits(script: &Script) {
    let baseline = script.check(&CallOptions::default()).unwrap();
    let stats = baseline.stats;
    for (steps, memory, expected) in [
        (stats.steps, stats.peak_memory_bytes, None),
        (
            stats.steps - 1,
            stats.peak_memory_bytes,
            Some(ErrorKind::Steps),
        ),
        (
            stats.steps,
            stats.peak_memory_bytes - 1,
            Some(ErrorKind::Memory),
        ),
    ] {
        let result = script.check(&limited(steps, memory));
        match (&result, expected) {
            (Ok(report), None) => same_report(report, &baseline),
            (Err(error), Some(kind)) => assert_eq!(error.kind, kind),
            _ => panic!("{result:?} for {expected:?}"),
        }
    }
    for sample in [1, 5, 9, 13] {
        let steps = stats.steps * sample / 16;
        let error = script
            .check(&limited(steps, stats.peak_memory_bytes))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        let memory = stats.peak_memory_bytes * sample as usize / 16;
        let error = script.check(&limited(stats.steps, memory)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
    }
}

#[test]
fn settled_contexts_and_join_caches_obey_exact_and_sampled_limits() {
    for source in [
        include_str!("site/rosettacode/popular/magic_squares_of_odd_order.vibe").to_string(),
        nested_loops(4),
    ] {
        exact_limits(&Engine::new().compile(&source).unwrap());
    }
}
