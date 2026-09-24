//! Lint checks for Vibescript programs, as `vibes analyze` reports them.
//!
//! The analyzer reports statements that can never run: anything after a
//! `return`, `raise`, `break`, `next` or `retry` in the same body, or after a
//! compound statement whose every path ends in one, such as an `if` whose
//! branches all return. Its scopes, positions and ordering match the
//! reference implementation's linter.

use vibescript::{Engine, Error, Position, Script, tooling};

/// The message of an unreachable-statement finding.
pub const UNREACHABLE: &str = "unreachable statement";

/// One lint finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finding {
    /// The enclosing scope: a function name, `<script>` for top-level code,
    /// `Class#method`, `Class.method` or `Class.<class body>`, with nested
    /// namespaces qualified as `Outer::Inner`. Each enclosing block literal
    /// appends ` block at LINE:COLUMN`, the position of its `do` or `{`.
    pub function: String,
    /// The one-based line and column of the statement.
    pub position: Position,
    pub message: String,
}

/// Compiles `source` without module paths or host bindings and returns its
/// findings, sorted by position and then by scope.
///
/// Compilation errors are returned unchanged. Nothing is executed.
///
/// ```
/// use vibescript_tools::analyze::{analyze, UNREACHABLE};
/// let findings = analyze("def run()\n  return 1\n  2\nend")?;
/// assert_eq!(findings.len(), 1);
/// assert_eq!(findings[0].function, "run");
/// assert_eq!((findings[0].position.line, findings[0].position.column), (3, 3));
/// assert_eq!(findings[0].message, UNREACHABLE);
/// assert!(analyze("def run()\n  1\nend")?.is_empty());
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn analyze(source: &str) -> Result<Vec<Finding>, Error> {
    analyze_script(&Engine::new().compile(source)?)
}

/// Returns the findings of an already compiled script, sorted by position
/// and then by scope.
///
/// ```
/// use vibescript::Engine;
/// use vibescript_tools::analyze::analyze_script;
/// let script = Engine::new().compile("[1].each do |x|\n  raise \"boom\"\n  x\nend")?;
/// let findings = analyze_script(&script)?;
/// assert_eq!(findings[0].function, "<script> block at 1:10");
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn analyze_script(script: &Script) -> Result<Vec<Finding>, Error> {
    Ok(tooling::unreachable(script.source())?
        .into_iter()
        .map(|unreachable| Finding {
            function: unreachable.function,
            position: unreachable.position,
            message: UNREACHABLE.to_owned(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::analyze;

    fn scopes(source: &str) -> Vec<String> {
        analyze(source)
            .unwrap()
            .into_iter()
            .map(|f| format!("{}:{} {}", f.position.line, f.position.column, f.function))
            .collect()
    }

    /// The reference's `TestAnalyzeCommand` cases.
    #[test]
    fn reports_the_reference_cases() {
        assert!(scopes("def run()\n  value = 1\n  value\nend").is_empty());
        assert!(scopes("def double(x)\n  x * 2\nend\n\ndouble(3)").is_empty());
        assert_eq!(scopes("def run()\n  return 1\n  2\nend"), ["3:3 run"]);
        assert_eq!(
            scopes(
                "def run()\n  if false\n    return 1\n  elsif true\n    return 2\n  else\n    return 3\n  end\n  4\nend"
            ),
            ["9:3 run"]
        );
        assert_eq!(
            scopes("def run()\n  begin\n    return 1\n  ensure\n    value = 2\n  end\n  3\nend"),
            ["7:3 run"]
        );
        assert_eq!(
            scopes(
                "def run()\n  begin\n    1\n  rescue\n    2\n  else\n    return 3\n    4\n  end\nend"
            ),
            ["8:5 run"]
        );
        assert_eq!(
            scopes(
                "def run()\n  begin\n    1\n  rescue\n    return 2\n  else\n    return 3\n  end\n  4\nend"
            ),
            ["9:3 run"]
        );
        assert_eq!(
            scopes(
                "class Reporter\n  def instance_path()\n    return 1\n    2\n  end\n\n  def self.class_path()\n    return 3\n    4\n  end\nend\n\ndef run()\n  Reporter.new.instance_path\nend"
            ),
            ["4:5 Reporter#instance_path", "9:5 Reporter.class_path"]
        );
        assert_eq!(
            scopes("def run()\n  [1].each do |x|\n    raise \"boom\"\n    x\n  end\nend"),
            ["4:5 run block at 2:12"]
        );
        assert_eq!(
            scopes("def run()\n  %I[#{capture { raise \"boom\"; 1 }}]\nend"),
            ["1:25 run block at 1:9"]
        );
        assert_eq!(
            scopes(
                "class Reporter\n  raise \"boom\"\n  1\n\n  def value()\n    2\n  end\nend\n\ndef run()\n  Reporter.new.value\nend"
            ),
            ["3:3 Reporter.<class body>"]
        );
    }

    #[test]
    fn follows_the_reference_positions_and_scopes() {
        assert_eq!(
            scopes(
                "def f\n  return 1\n  x = 2 if true\n  y = 3 unless false\n  z while false\nend"
            ),
            ["3:9 f", "4:9 f", "5:5 f"]
        );
        assert_eq!(scopes("def f\n  return\n  a == b\nend"), ["3:6 f"]);
        assert_eq!(scopes("def f\n  return\n  a <=> b\nend"), ["3:5 f"]);
        assert_eq!(scopes("def f\n  return\n  x rescue y\nend"), ["3:5 f"]);
        assert_eq!(scopes("def f\n  return\n  -1\nend"), ["3:4 f"]);
        assert_eq!(
            scopes("raise \"x\"\nclass A\n  1\nend\nclass B\n  def m\n  end\nend"),
            ["2:1 <script>"]
        );
        assert_eq!(
            scopes("puts 1\nclass A\n  raise \"x\"\n  1\nend"),
            ["4:3 <script>", "4:3 A.<class body>"]
        );
        assert_eq!(
            scopes("def f\n  return\n  1\nend\nalias g f"),
            ["3:3 f", "3:3 g"]
        );
    }
}
