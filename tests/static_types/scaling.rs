//! Checking is linear: each function is checked once, from its signature and
//! those of what it calls, so doubling a program at most doubles the work.

use vibescript::Engine;

fn repeat(count: usize, item: impl Fn(usize) -> String) -> String {
    (0..count).map(item).collect()
}

/// A description, a repetition count and a program repeating a declaration,
/// statement or expression.
type Shape = (&'static str, usize, fn(usize) -> String);

const SHAPES: [Shape; 17] = [
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
    (
        "calls on a union receiver nested in their arguments",
        60,
        |count| {
            let mut argument = "1".to_owned();
            for _ in 0..count {
                argument = format!("u.first({argument}).length");
            }
            format!("def f(u: array<int> | array<int?>) -> int\n  {argument}\nend\n")
        },
    ),
    ("nested hash literals", 100, |count| {
        let mut value = "1".to_owned();
        for _ in 0..count {
            value = format!("{{ a: {value} }}");
        }
        format!(
            "def f -> any
  x = {value}
  x
end\n"
        )
    }),
    (
        "compound assignments through nested receivers",
        100,
        |count| {
            format!(
                "def f(rows: array<array<int>>) -> int\n{}  0\nend\n",
                repeat(count, |i| format!(
                    "  rows.fetch({i})[0] = rows.fetch({i}).fetch(0) + 1\n"
                ))
            )
        },
    ),
    ("begins nested around repeated assignments", 100, |count| {
        nest(count, "begin\n", "rescue\n  c = 1\nensure\n  c = 2\nend\n")
    }),
    ("loops nested around repeated assignments", 100, |count| {
        nest(count, "while c > 0\n", "  c -= 1\nend\n")
    }),
    ("blocks nested around repeated assignments", 60, |count| {
        nest(count, "[1].each { |q|\n", "}\n")
    }),
    ("loops assigning many locals", 100, |count| {
        format!(
            "def f(x: int) -> int\n{}  while x > 0\n{}    x -= 1\n  end\n  0\nend\n",
            repeat(count, |i| format!("  v{i} = {i}\n")),
            repeat(count, |i| format!("    v{i} = x\n"))
        )
    }),
];

/// A function whose body nests `count` levels of `open` and `close` around
/// `count` assignments of one local.
fn nest(count: usize, open: &str, close: &str) -> String {
    format!(
        "def f -> int\n  x: int? = 0\n  c = 1\n{}{}{}  0\nend\n",
        open.repeat(count),
        "x = 1\n".repeat(count),
        close.repeat(count)
    )
}

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
        // WASI checks syntax at most 128 levels tall, which the nesting
        // shapes exceed at full size.
        let count = if cfg!(target_os = "wasi") {
            count / 8
        } else {
            count
        };
        let small = steps(&shape(count));
        let large = steps(&shape(2 * count));
        assert!(
            large * 10 <= small * 22,
            "{name}: {small} steps for {count}, {large} for {}",
            2 * count
        );
    }
}

#[test]
fn a_deep_nest_around_many_assignments_checks_in_linear_work() {
    // Each level's rescue and ensure may see what the levels inside assign;
    // listing those assignments again at every level once took work and
    // memory proportional to the depth times the assignments.
    let levels = if cfg!(target_os = "wasi") { 100 } else { 900 };
    let source = format!(
        "x: int? = 1\nc = true\n{}{}{}",
        "begin\n".repeat(levels),
        "x = 1\n".repeat(10_000),
        "rescue\nc = false\nensure\nc = true\nend\n".repeat(levels)
    );
    let steps = steps(&source);
    assert!(
        steps < 2 * source.len() as u64,
        "{steps} steps for {} bytes",
        source.len()
    );
}

// WASI preview 1 cannot start the small-stack thread.
#[cfg(not(target_os = "wasi"))]
#[test]
fn syntax_as_deep_as_the_parser_allows_checks_on_a_small_stack() {
    let chain = format!(
        "def f(s: string) -> string\n  s{}\nend\n",
        ".upcase".repeat(1000)
    );
    let mut sum = "1".to_owned();
    for _ in 0..1000 {
        sum = format!("({sum} + 1)");
    }
    let sum = format!("def f -> int\n  {sum}\nend\n");
    std::thread::Builder::new()
        .stack_size(512 << 10)
        .spawn(move || {
            for source in [chain, sum] {
                let checked = Engine::new().type_check(&source).unwrap();
                assert!(
                    checked.diagnostics.is_empty(),
                    "{:?}",
                    checked.diagnostics.first()
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
