//! Whole-file checking cost grows with the program for shapes that once cost quadratically or
//! cubically more steps: every declaration check or call copied all namespace state, a new
//! initializer restarted the top-level analysis, sequential branches walked every later join
//! again, and several lookups scanned every earlier declaration.

use vibescript::{CallOptions, Engine, Limits};

fn repeat(count: usize, item: impl Fn(usize) -> String) -> String {
    (0..count).map(item).collect()
}

/// A description, a repetition count and a program repeating a declaration or statement.
type Shape = (&'static str, usize, fn(usize) -> String);

const SHAPES: [Shape; 8] = [
    ("classes with a constant", 100, |count| {
        repeat(count, |i| format!("class P{i}\n  CONST = \"x{i}\"\nend\n"))
    }),
    ("empty classes", 100, |count| {
        repeat(count, |i| format!("class P{i}\nend\n"))
    }),
    ("modules with a method", 100, |count| {
        repeat(count, |i| {
            format!("module M{i}\n  def self.f\n    1\n  end\nend\n")
        })
    }),
    ("enums", 100, |count| {
        repeat(count, |i| format!("enum E{i}\n  A\n  B\nend\n"))
    }),
    ("methods of one class", 100, |count| {
        format!(
            "class C\n{}end\n",
            repeat(count, |i| format!("  def m{i}\n    {i}\n  end\n"))
        )
    }),
    ("calls with distinct arguments", 100, |count| {
        format!(
            "def g(a)\n  a\nend\ndef f\n{}end\n",
            repeat(count, |i| format!("  g({i})\n"))
        )
    }),
    ("sequential branches assigning literals", 100, |count| {
        format!(
            "def f(x)\n  y = 0\n{}  y\nend\n",
            repeat(count, |i| format!("  if x == {i}\n    y = {i}\n  end\n"))
        )
    }),
    ("rendered operands", 1000, |count| {
        format!(
            "class Blank\n  def to_s\n    \"\"\n  end\nend\ndef run(v)\n  a = Blank.new\n  \
             format(\"\", {})\nend\n",
            vec!["a, v"; count / 2].join(", ")
        )
    }),
];

fn steps(source: &str) -> u64 {
    let script = Engine::new().compile(source).unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: None,
            memory_bytes: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let report = script.check(&options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    report.stats.steps
}

#[test]
fn doubling_repeated_declarations_and_statements_at_most_doubles_checking_steps() {
    for (name, count, shape) in SHAPES {
        let small = steps(&shape(count));
        let large = steps(&shape(2 * count));
        // Linear growth doubles the steps; quadratic growth quadruples them. The margin
        // allows fixed costs and the logarithmic depth of persistent state tables.
        assert!(
            large * 10 <= small * 24,
            "{name}: {small} steps for {count}, {large} for {}",
            2 * count
        );
    }
}
