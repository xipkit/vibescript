//! `vibes fix` and `vibes migrate` on the gaps the documentation migration
//! found.

use vibescript::{CallOptions, Engine};
use vibescript_tools::fix::fix;

/// Fixes `source` as `vibes fix` does, and returns the fixed text.
fn fixed(source: &str) -> String {
    let engine = Engine::new();
    let result = fix(source, |text| {
        engine.type_check(text).map(|checked| checked.diagnostics)
    })
    .unwrap();
    let remaining: Vec<String> = result
        .remaining
        .iter()
        .filter(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| diagnostic.render(&result.source))
        .collect();
    assert!(remaining.is_empty(), "{}", remaining.concat());
    result.source
}

/// The value of `run` in `source`, compiled with static types.
fn run(source: &str) -> String {
    let mut engine = Engine::new();
    engine.set_static_types(true);
    engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}\n{error}"))
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value
        .to_string()
}

#[test]
fn fixing_reads_leaves_writes_through_elements_in_place() {
    let source = "def run -> array<any>
  grid = [[1, 2], [3, 4]]
  grid[0][1] = 9
  grid[1] << 5
  grid[0][0] += 1
  corner: int = grid[1][0]
  [grid, corner]
end
";
    let result = fixed(source);
    assert_eq!(
        result,
        "def run -> array<any>
  grid = [[1, 2], [3, 4]]
  grid[0][1] = 9
  grid[1] << 5
  grid[0][0] = grid.fetch(0).fetch(0) + 1
  corner: int = grid.fetch(1).fetch(0)
  [grid, corner]
end
"
    );
    assert_eq!(run(&result), "[[[2, 9], [3, 4, 5]], 3]");
}
