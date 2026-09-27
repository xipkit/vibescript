//! Type-changing edits of the repository's own programs: the language
//! corpus's cases and the site, upstream and glue programs. An edit widens
//! or changes a declared type, flips a nil test, or swaps a literal for one
//! of another type; the checker must reject what the runtime then gets
//! wrong.

use super::{harness::Case, rng::Rng};
use std::{path::Path, sync::OnceLock};

/// The programs edits start from.
pub fn corpus() -> &'static [String] {
    static CORPUS: OnceLock<Vec<String>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = Vec::new();
        if let Ok(text) = std::fs::read_to_string(root.join("tests/language.json")) {
            if let Ok(serde_json::Value::Array(cases)) =
                serde_json::from_str::<serde_json::Value>(&text)
            {
                for case in cases {
                    if let Some(source) = case["source"].as_str() {
                        // A case's function runs when the script calls it.
                        if source.contains("def run(input: any)") && deterministic(source) {
                            sources.push(format!("{source}\np(run(nil))\n"));
                        }
                    }
                }
            }
        }
        for directory in ["tests/site", "tests/upstream", "corpus/glue"] {
            collect(&root.join(directory), &mut sources);
        }
        sources
    })
}

fn collect(directory: &Path, sources: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(&path, sources);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "vibe")
        {
            // Programs that require files are left out; their files are not
            // written with them.
            if let Ok(source) = std::fs::read_to_string(&path) {
                if !source.contains("require") && deterministic(&source) {
                    sources.push(source);
                }
            }
        }
    }
}

/// Whether two runs of `source` see the same values: the clock and
/// time-ordered identifiers differ between them.
fn deterministic(source: &str) -> bool {
    !["uuid", "Time.now", ".ago", "from_now"]
        .iter()
        .any(|word| source.contains(word))
}

/// An edited corpus program from `seed`, or `None` when no edit applies.
pub fn program(seed: u64) -> Option<Case> {
    let corpus = corpus();
    if corpus.is_empty() {
        return None;
    }
    let mut rng = Rng::new(seed ^ 0x6d75_7461_7465);
    let mut source = corpus[rng.below(corpus.len())].clone();
    let mut edited = false;
    for _ in 0..1 + rng.below(3) {
        if let Some(next) = edit(&source, &mut rng) {
            source = next;
            edited = true;
        }
    }
    edited.then(|| Case::new(source))
}

/// Each edit: the text it looks for, and what may replace it.
const EDITS: &[(&str, &[&str])] = &[
    (
        ": int",
        &[": int?", ": number", ": any", ": float", ": int | string"],
    ),
    (
        ": string",
        &[": string?", ": symbol", ": any", ": string | int"],
    ),
    (": float", &[": float?", ": number", ": int"]),
    (": bool", &[": bool?", ": any"]),
    ("-> int", &["-> int?", "-> number", "-> any"]),
    ("-> string", &["-> string?", "-> any"]),
    ("-> array<", &["-> array<any> | array<"]),
    (
        "array<int>",
        &["array<int?>", "array<any>", "array<number>"],
    ),
    (
        "array<string>",
        &["array<string?>", "array<any>", "array<symbol>"],
    ),
    (
        "hash<string, int>",
        &["hash<string, int?>", "hash<string, any>"],
    ),
    (
        "hash<string, string>",
        &["hash<string, string?>", "hash<string, any>"],
    ),
    ("!= nil", &["== nil"]),
    ("== nil", &["!= nil"]),
    (".fetch(0)", &[".first", ".last"]),
    (".first", &[".last", "[0]", "[-1]"]),
    ("true", &["false", "nil"]),
    ("0", &["nil", "0.0", "\"0\""]),
    ("1", &["-1", "1.5", "nil"]),
    ("\"a\"", &[":a", "nil", "1"]),
    ("[]", &["[nil]", "[1]", "[\"a\"]"]),
    ("{}", &["{ a: 1 }", "{ a: nil }"]),
    (" if ", &[" if !"]),
    ("return ", &["return nil if false\n  return "]),
];

/// One edit at a random place, when the source has a place for it.
fn edit(source: &str, rng: &mut Rng) -> Option<String> {
    for _ in 0..8 {
        let (pattern, replacements) = EDITS[rng.below(EDITS.len())];
        let places: Vec<usize> = source.match_indices(pattern).map(|(at, _)| at).collect();
        if places.is_empty() {
            continue;
        }
        let at = places[rng.below(places.len())];
        let replacement = replacements[rng.below(replacements.len())];
        let mut edited = String::with_capacity(source.len() + replacement.len());
        edited.push_str(&source[..at]);
        edited.push_str(replacement);
        edited.push_str(&source[at + pattern.len()..]);
        return Some(edited);
    }
    None
}
