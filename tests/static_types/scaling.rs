//! Checking is linear: each function is checked once, from its signature and
//! those of what it calls, so doubling a program at most doubles the work.

use vibescript::Engine;

fn repeat(count: usize, item: impl Fn(usize) -> String) -> String {
    (0..count).map(item).collect()
}

/// A description, a repetition count and a program repeating a declaration,
/// statement or expression.
type Shape = (&'static str, usize, fn(usize) -> String);

const SHAPES: [Shape; 11] = [
    ("functions calling their predecessor", 200, |count| {
        "def f0(n: int) -> int\n  n\nend\n".to_owned()
            + &repeat(count, |i| {
                format!("def f{}(n: int) -> int\n  f{i}(n) + 1\nend\n", i + 1)
            })
    }),
    ("classes with methods and ivars", 100, |count| {
        repeat(count, |i| {
            format!("class P{i}\n  @v: int = {i}\n  def get -> int\n    @v\n  end\nend\n")
        })
    }),
    ("sequential branches narrowing one local", 200, |count| {
        format!(
            "def f(x: int?) -> int\n  y = 0\n{}  y\nend\n",
            repeat(count, |i| format!(
                "  if x != nil\n    y = x + {i}\n  end\n"
            ))
        )
    }),
    ("nested branches", 60, |count| {
        let mut body = "y = 1\n".to_owned();
        for i in 0..count {
            body = format!("if x > {i}\n{body}else\ny = {i}\nend\n");
        }
        format!("def f(x: int) -> int\n  y = 0\n{body}  y\nend\n")
    }),
    ("many locals then many branches", 100, |count| {
        format!(
            "def f(x: int) -> int\n{}{}  0\nend\n",
            repeat(count, |i| format!("  v{i} = {i}\n")),
            repeat(count, |i| format!("  if x > {i}\n    v{i} = 1\n  end\n"))
        )
    }),
    ("a long method chain", 200, |count| {
        format!(
            "def f(s: string) -> string\n  s{}\nend\n",
            ".upcase".repeat(count)
        )
    }),
    ("nested blocks", 60, |count| {
        let mut body = "x".to_owned();
        for _ in 0..count {
            body = format!("[x].map {{ |x| {body} }}.fetch(0)");
        }
        format!("def f(x: int) -> int\n  {body}\nend\n")
    }),
    ("a wide array literal", 500, |count| {
        format!(
            "def f -> array<int>\n  [{}]\nend\n",
            repeat(count, |i| format!("{i}, "))
        )
    }),
    ("one branch assigning many locals", 200, |count| {
        format!(
            "def f(x: int) -> int\n{}  if x > 0\n{}  end\n  0\nend\n",
            repeat(count, |i| format!("  v{i} = {i}\n")),
            repeat(count, |i| format!("    v{i} = x\n"))
        )
    }),
    ("a block with many breaks", 200, |count| {
        format!(
            "def f(xs: array<int>) -> int\n  y = 0\n  xs.each {{ |x|\n{}  }}\n  y\nend\n",
            repeat(count, |i| format!("    y = {i}\n    break if x == {i}\n"))
        )
    }),
    ("loops assigning many locals", 100, |count| {
        format!(
            "def f(x: int) -> int\n{}  while x > 0\n{}    x -= 1\n  end\n  0\nend\n",
            repeat(count, |i| format!("  v{i} = {i}\n")),
            repeat(count, |i| format!("    v{i} = x\n"))
        )
    }),
];

fn steps(source: &str) -> u64 {
    let checked = Engine::new().type_check(source).unwrap();
    assert!(
        checked.diagnostics.is_empty(),
        "{source}\n{:?}",
        checked.diagnostics.first()
    );
    checked.steps
}

#[test]
fn doubling_a_program_at_most_doubles_the_checking_work() {
    for (name, count, shape) in SHAPES {
        let small = steps(&shape(count));
        let large = steps(&shape(2 * count));
        assert!(
            large * 10 <= small * 22,
            "{name}: {small} steps for {count}, {large} for {}",
            2 * count
        );
    }
}
