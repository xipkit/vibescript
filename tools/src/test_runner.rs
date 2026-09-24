//! Test discovery and execution, as `vibes test` performs them.
//!
//! A test file is named `*_test.vibe` and declares only functions, classes,
//! modules, enums and aliases: executable top-level statements are rejected,
//! as the reference compiler rejects them. A test is a top-level function
//! whose name starts with `test_`; it passes when it returns and fails when
//! it raises, including a failed `assert`. Test functions must not require
//! arguments. Every test runs as its own call with fresh script state.
//!
//! [`discover`] finds test files, and [`run`] or [`run_each`] runs the tests
//! of a compiled script and returns structured results, leaving rendering to
//! the caller.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};
use vibescript::tooling::{self, ItemKind, StatementKind};
use vibescript::{CallOptions, Engine, Error, Limits, Script, Value};

/// The prefix that marks a test function.
pub const TEST_PREFIX: &str = "test_";

/// The suffix that marks a test file.
pub const TEST_SUFFIX: &str = "_test.vibe";

/// Why discovery stopped.
#[derive(Debug)]
pub enum DiscoverError {
    /// A root could not be inspected.
    Access { root: PathBuf, error: io::Error },
    /// A root names a file that is not a `*_test.vibe` file.
    NotTestFile { root: PathBuf },
    /// A directory below a root could not be read.
    Walk {
        root: PathBuf,
        directory: PathBuf,
        error: io::Error,
    },
}

/// Reports whether a path's file name ends in `_test.vibe`.
///
/// ```
/// use vibescript_tools::test_runner::is_test_file;
/// assert!(is_test_file("billing/fees_test.vibe".as_ref()));
/// assert!(!is_test_file("billing/fees.vibe".as_ref()));
/// ```
pub fn is_test_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(TEST_SUFFIX))
}

/// Expands roots into a sorted, deduplicated list of test files.
///
/// Directories are walked recursively without following linked
/// directories, and paths keep the spelling of their root, cleaned
/// lexically, so a root of `.` yields `fees_test.vibe` rather than
/// `./fees_test.vibe`. A root that is a file must follow the naming
/// convention. Paths sort by their bytes.
///
/// ```
/// use vibescript_tools::test_runner::discover;
/// # let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(".cache/tmp");
/// let dir = base.join("discover-example");
/// # let _ = std::fs::remove_dir_all(&dir);
/// std::fs::create_dir_all(dir.join("nested"))?;
/// std::fs::write(dir.join("nested/b_test.vibe"), "")?;
/// std::fs::write(dir.join("a_test.vibe"), "")?;
/// std::fs::write(dir.join("helper.vibe"), "")?;
/// let files = discover(&[dir.clone()]).unwrap();
/// assert_eq!(files, [dir.join("a_test.vibe"), dir.join("nested/b_test.vibe")]);
/// std::fs::remove_dir_all(&dir)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn discover(roots: &[PathBuf]) -> Result<Vec<PathBuf>, DiscoverError> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        let path = clean(&path);
        if !files.contains(&path) {
            files.push(path);
        }
    };
    for root in roots {
        let metadata = fs::metadata(root).map_err(|error| DiscoverError::Access {
            root: root.clone(),
            error,
        })?;
        if !metadata.is_dir() {
            if !is_test_file(root) {
                return Err(DiscoverError::NotTestFile { root: root.clone() });
            }
            add(root.clone());
            continue;
        }
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let walk = |error| DiscoverError::Walk {
                root: root.clone(),
                directory: directory.clone(),
                error,
            };
            for entry in fs::read_dir(&directory).map_err(walk)? {
                let entry = entry.map_err(walk)?;
                let path = clean(&directory.join(entry.file_name()));
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(path);
                } else if is_test_file(&path) {
                    add(path);
                }
            }
        }
    }
    files.sort_by(|a, b| {
        a.as_os_str()
            .as_encoded_bytes()
            .cmp(b.as_os_str().as_encoded_bytes())
    });
    Ok(files)
}

/// Lexically cleans a path as Go's `filepath.Clean` does.
fn clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(component),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

/// Selects tests by a regular expression in the engine's Go-compatible syntax.
pub struct Filter {
    pattern: Value,
    script: Script,
}

impl Filter {
    /// Compiles a pattern, returning the regular-expression error text on failure.
    ///
    /// ```
    /// use vibescript_tools::test_runner::Filter;
    /// let filter = Filter::new("al.ha").unwrap();
    /// assert!(filter.matches("test_alpha").unwrap());
    /// assert!(!filter.matches("test_beta").unwrap());
    /// assert_eq!(
    ///     Filter::new("[").err().unwrap(),
    ///     "error parsing regexp: missing closing ]: `[`"
    /// );
    /// ```
    pub fn new(pattern: &str) -> Result<Self, String> {
        let script = Engine::new()
            .compile("def matches(pattern, name)\n  Regex.match(pattern, name) != nil\nend\n")
            .map_err(|error| error.to_string())?;
        let filter = Self {
            pattern: Value::bytes(pattern),
            script,
        };
        filter.matches("").map(|_| filter)
    }

    /// Reports whether the pattern matches anywhere in `name`.
    pub fn matches(&self, name: &str) -> Result<bool, String> {
        let options = CallOptions {
            limits: Limits {
                steps: None,
                memory_bytes: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        self.script
            .call(
                "matches",
                &[self.pattern.clone(), Value::bytes(name)],
                options,
            )
            .map(|outcome| outcome.value.truthy())
            .map_err(|error| match error.message.split_once("invalid regex: ") {
                Some((_, reason)) => reason.to_owned(),
                None => error.message,
            })
    }
}

/// How [`run`] selects and calls tests.
#[derive(Default)]
pub struct Options {
    /// Runs only tests whose names match.
    pub filter: Option<Filter>,
    /// Limits, cancellation and globals for each test call.
    pub call: CallOptions,
}

/// Why a test file's tests could not run at all.
#[derive(Debug)]
pub enum SuiteError {
    /// The file executes code at the top level, which test files must not do.
    TopLevelStatement(StatementKind),
    /// Reading the file's declarations failed.
    Outline(Error),
    /// Applying the filter failed.
    Filter(String),
    /// The call options were cancelled after a test failed.
    Cancelled,
}

/// Why one test failed.
#[derive(Debug)]
pub enum TestFailure {
    /// The test function has a required parameter.
    RequiresArguments,
    /// The test raised, or an `assert` in it failed.
    Error(Error),
}

/// The result of one test function.
#[derive(Debug)]
pub struct TestOutcome {
    pub name: String,
    /// `None` when the test passed.
    pub failure: Option<TestFailure>,
}

/// Runs the selected tests of a compiled test file, in name order, and
/// returns their outcomes.
///
/// An empty result means the file has no selected test functions.
///
/// ```
/// use vibescript::Engine;
/// use vibescript_tools::test_runner::{run, Options, TestFailure};
/// let script = Engine::new().compile(
///     "def test_sum\n  assert 1 + 2 == 3\nend\n\
///      def test_broken\n  assert 1 == 2, \"one is not two\"\nend\n\
///      def helper\n  1\nend\n",
/// )?;
/// let outcomes = run(&script, &Options::default()).unwrap();
/// assert_eq!(outcomes[0].name, "test_broken");
/// match &outcomes[0].failure {
///     Some(TestFailure::Error(error)) => assert_eq!(error.message, "one is not two"),
///     other => panic!("{other:?}"),
/// }
/// assert_eq!(outcomes[1].name, "test_sum");
/// assert!(outcomes[1].failure.is_none());
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn run(script: &Script, options: &Options) -> Result<Vec<TestOutcome>, SuiteError> {
    let mut outcomes = Vec::new();
    run_each(script, options, |outcome| outcomes.push(outcome))?;
    Ok(outcomes)
}

/// Runs the selected tests of a compiled test file, in name order, passing
/// each outcome to `report` as soon as its test finishes, so output the
/// tests write interleaves with their results. Returns how many tests ran.
///
/// ```
/// use vibescript::Engine;
/// use vibescript_tools::test_runner::{run_each, Options};
/// let script = Engine::new().compile("def test_a\n  assert true\nend\ndef test_b\nend\n")?;
/// let mut names = Vec::new();
/// let count = run_each(&script, &Options::default(), |outcome| names.push(outcome.name)).unwrap();
/// assert_eq!((count, names), (2, vec!["test_a".to_owned(), "test_b".to_owned()]));
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn run_each(
    script: &Script,
    options: &Options,
    mut report: impl FnMut(TestOutcome),
) -> Result<usize, SuiteError> {
    let outline = tooling::outline(script.source()).map_err(SuiteError::Outline)?;
    let mut functions = Vec::new();
    for item in &outline.items {
        match (item.kind, &item.function) {
            (ItemKind::Statement(kind), _) => return Err(SuiteError::TopLevelStatement(kind)),
            (ItemKind::Function | ItemKind::Alias, Some(function)) => {
                functions.push((&item.name, function.requires_arguments()));
            }
            _ => (),
        }
    }
    functions.sort();
    let mut selected = Vec::new();
    for (name, requires_arguments) in functions {
        if !name.starts_with(TEST_PREFIX) {
            continue;
        }
        if let Some(filter) = &options.filter {
            if !filter.matches(name).map_err(SuiteError::Filter)? {
                continue;
            }
        }
        selected.push((name, requires_arguments));
    }
    for &(name, requires_arguments) in &selected {
        let failure = if requires_arguments {
            Some(TestFailure::RequiresArguments)
        } else {
            match script.call(name, &[], options.call.clone()) {
                Ok(_) => None,
                Err(_) if options.call.cancellation.is_cancelled() => {
                    return Err(SuiteError::Cancelled);
                }
                Err(error) => Some(TestFailure::Error(error)),
            }
        };
        report(TestOutcome {
            name: name.clone(),
            failure,
        });
    }
    Ok(selected.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcomes(source: &str) -> Result<Vec<(String, Option<String>)>, SuiteError> {
        let script = Engine::new().compile(source).unwrap();
        run(&script, &Options::default()).map(|outcomes| {
            outcomes
                .into_iter()
                .map(|outcome| {
                    let failure = outcome.failure.map(|failure| match failure {
                        TestFailure::RequiresArguments => "requires arguments".to_owned(),
                        TestFailure::Error(error) => error.message,
                    });
                    (outcome.name, failure)
                })
                .collect()
        })
    }

    #[test]
    fn runs_test_functions_and_reports_failures() {
        assert_eq!(
            outcomes(
                "def test_addition()\n  assert 1 + 2 == 3\nend\n\
                 def test_subtraction()\n  assert 5 - 2 == 3, \"subtraction is broken\"\nend\n\
                 def helper()\n  \"not a test\"\nend\n"
            )
            .unwrap(),
            [
                ("test_addition".to_owned(), None),
                ("test_subtraction".to_owned(), None)
            ]
        );
        assert_eq!(
            outcomes("def test_failure()\n  assert 1 == 2, \"one is not two\"\nend\n").unwrap(),
            [("test_failure".to_owned(), Some("one is not two".to_owned()))]
        );
    }

    #[test]
    fn rejects_required_parameters_but_allows_defaults() {
        assert_eq!(
            outcomes(
                "def test_needs_arg(value)\n  assert value\nend\n\
                 def test_default_ok(value = 1)\n  assert value == 1\nend\n"
            )
            .unwrap(),
            [
                ("test_default_ok".to_owned(), None),
                (
                    "test_needs_arg".to_owned(),
                    Some("requires arguments".to_owned())
                )
            ]
        );
    }

    #[test]
    fn rejects_top_level_statements() {
        for (source, kind) in [
            ("puts 1\ndef test_a\nend\n", StatementKind::Expression),
            ("x = 1\n", StatementKind::Assignment),
            ("until true\nend\n", StatementKind::Until),
            ("return if true\n", StatementKind::If),
            ("begin\n1\nend\n", StatementKind::Begin),
        ] {
            match outcomes(source) {
                Err(SuiteError::TopLevelStatement(found)) => assert_eq!(found, kind, "{source}"),
                other => panic!("{source}: {other:?}"),
            }
        }
        assert!(
            outcomes("class A\n  X = 1\nend\nenum E\n  A\nend\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn filters_tests_by_regular_expression() {
        let script = Engine::new()
            .compile("def test_alpha()\n  assert true\nend\ndef test_beta()\n  assert false\nend\n")
            .unwrap();
        let options = Options {
            filter: Some(Filter::new("alpha").unwrap()),
            ..Options::default()
        };
        let outcomes = run(&script, &options).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].name, "test_alpha");
        assert_eq!(
            Filter::new("a(").err().unwrap(),
            "error parsing regexp: missing closing ): `a(`"
        );
    }
}
