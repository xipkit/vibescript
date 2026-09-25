//! Compiles every Vibescript example in the documentation with static types.
//!
//! Each fenced ```` ```vibe ```` block in `README.md`, `docs/` and the
//! builtin reference in `tools/src/lsp/reference/` must compile with
//! [`Engine::set_static_types`] and produce no diagnostics, warnings
//! included. The fence's info string may add attributes, separated by spaces:
//!
//! - `error=V0101` marks a deliberately invalid example. It must fail to
//!   compile with exactly the listed codes, separated by commas; a syntax
//!   error is `V0001`.
//! - `module=reports/format.vibe` makes the block a required file of that
//!   name for the later blocks in the same document. The block itself is
//!   checked as a required file.
//! - `global=name:type` declares a host global, as
//!   [`Engine::declare_global`] does, for this block only. The type is
//!   written without spaces.
//!
//! Architecture decision records keep the language they were written in and
//! are not checked. Blocks fenced as `vibescript` are refused, so that no
//! example escapes the check under another name.

mod common;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use vibescript::{Engine, ErrorKind, ModuleConfig};

/// One fenced example.
struct Block {
    /// The one-based line of the block's first source line.
    line: usize,
    info: String,
    source: String,
}

/// What a block's info string asks for.
#[derive(Default)]
struct Attributes {
    errors: Option<BTreeSet<String>>,
    module: Option<String>,
    globals: Vec<(String, String)>,
}

#[test]
fn documentation_examples_compile_with_static_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    let mut checked = 0;
    for document in documents(root) {
        let name = document.strip_prefix(root).unwrap().display().to_string();
        let text = fs::read_to_string(&document).unwrap();
        let (blocks, problems) = fenced_blocks(&text);
        failures.extend(
            problems
                .into_iter()
                .map(|problem| format!("{name}: {problem}")),
        );
        let mut modules: Option<PathBuf> = None;
        for block in blocks {
            checked += 1;
            let at = format!("{name}:{}", block.line);
            if let Err(message) = check(&block, &mut modules) {
                failures.push(format!("{at}: {message}\n{}", block.source));
            }
        }
        if let Some(directory) = modules {
            let _ = fs::remove_dir_all(directory);
        }
    }
    assert!(checked >= 150, "found only {checked} examples");
    assert!(
        failures.is_empty(),
        "{} of {checked} documentation examples failed:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The Markdown files whose examples are checked, in a stable order.
fn documents(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![root.join("README.md")];
    markdown_files(&root.join("docs"), &mut found);
    markdown_files(&root.join("tools/src/lsp/reference"), &mut found);
    found.retain(|path| !path.starts_with(root.join("docs/adr")));
    found.sort();
    found
}

fn markdown_files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            markdown_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            found.push(path);
        }
    }
}

/// The `vibe` blocks of a document, with the indentation of their opening
/// fence removed, and the fences that cannot be checked. Other languages are
/// skipped.
fn fenced_blocks(text: &str) -> (Vec<Block>, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut problems = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some(info) = line.trim_start().strip_prefix("```") else {
            index += 1;
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        let start = index + 1;
        let Some(end) = (start..lines.len()).find(|&at| lines[at].trim_start().starts_with("```"))
        else {
            problems.push(format!("the fence at line {} is never closed", index + 1));
            break;
        };
        match info.split_whitespace().next().unwrap_or("") {
            "vibescript" => problems.push(format!(
                "line {}: fence Vibescript examples as `vibe`",
                index + 1
            )),
            "vibe" => {
                let mut source = String::new();
                for body in &lines[start..end] {
                    let cut = body.len() - body.trim_start().len();
                    source.push_str(&body[cut.min(indent)..]);
                    source.push('\n');
                }
                blocks.push(Block {
                    line: start + 1,
                    info: info.trim()["vibe".len()..].trim().to_owned(),
                    source,
                });
            }
            _ => (),
        }
        index = end + 1;
    }
    (blocks, problems)
}

fn attributes(info: &str) -> Result<Attributes, String> {
    let mut parsed = Attributes::default();
    for attribute in info.split_whitespace() {
        match attribute.split_once('=') {
            Some(("error", codes)) => {
                parsed.errors = Some(codes.split(',').map(str::to_owned).collect());
            }
            Some(("module", path)) => parsed.module = Some(path.to_owned()),
            Some(("global", declaration)) => {
                let (name, ty) = declaration
                    .split_once(':')
                    .ok_or_else(|| format!("`{attribute}` is not `global=name:type`"))?;
                parsed.globals.push((name.to_owned(), ty.to_owned()));
            }
            _ => return Err(format!("unknown fence attribute `{attribute}`")),
        }
    }
    Ok(parsed)
}

fn check(block: &Block, modules: &mut Option<PathBuf>) -> Result<(), String> {
    let attributes = attributes(&block.info)?;
    let source = match &attributes.module {
        Some(path) => {
            let directory = modules.get_or_insert_with(module_directory);
            let file = directory.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, &block.source).unwrap();
            // The module is checked as a required file, through a script
            // that requires it.
            let name = path.strip_suffix(".vibe").unwrap_or(path);
            format!("require({name:?})\n")
        }
        None => block.source.clone(),
    };
    let mut engine = Engine::new();
    engine.set_static_types(true);
    if let Some(directory) = modules {
        engine
            .set_module_config(ModuleConfig {
                paths: vec![directory.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
    }
    for (name, ty) in &attributes.globals {
        engine
            .declare_global(name, ty)
            .map_err(|error| format!("cannot declare `{name}: {ty}`: {error}"))?;
    }
    let found = diagnostics(&engine, &source)?;
    let codes: BTreeSet<String> = found.iter().map(|(code, _)| code.clone()).collect();
    let report = || {
        if found.is_empty() {
            return "  none".to_owned();
        }
        found
            .iter()
            .map(|(code, message)| format!("  {code}: {message}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match attributes.errors {
        None if found.is_empty() => Ok(()),
        None => Err(format!("expected no diagnostics, found\n{}", report())),
        Some(expected) if codes == expected => Ok(()),
        Some(expected) => Err(format!("expected {expected:?}, found\n{}", report())),
    }
}

/// Every diagnostic compiling `source` reports, warnings included, as its
/// code and message; a syntax error is `V0001`.
fn diagnostics(engine: &Engine, source: &str) -> Result<Vec<(String, String)>, String> {
    let listed = |diagnostics: &[vibescript::diagnostic::Diagnostic]| {
        diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code.to_string(), diagnostic.message.clone()))
            .collect()
    };
    match engine.compile(source) {
        Ok(_) => {
            let checked = engine
                .type_check(source)
                .map_err(|error| error.to_string())?;
            Ok(listed(&checked.diagnostics))
        }
        Err(error) if !error.diagnostics().is_empty() => Ok(listed(error.diagnostics())),
        Err(error) if error.kind == ErrorKind::Syntax => {
            Ok(vec![("V0001".to_owned(), error.message.clone())])
        }
        Err(error) => Err(format!("compile failed without a diagnostic: {error}")),
    }
}

/// A fresh directory for one document's required files.
fn module_directory() -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
    fs::create_dir_all(&base).unwrap();
    for attempt in 0.. {
        let path = base.join(format!("docs-{}-{attempt}", common::process_id()));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create a module directory: {error}"),
        }
    }
    unreachable!()
}
