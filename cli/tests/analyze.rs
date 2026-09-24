//! `vibes analyze`, ported from the Go reference's TestAnalyzeCommand.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes};

#[test]
fn reports_unreachable_statements_like_the_reference() {
    let files = Files::new();
    for (source, findings) in [
        ("def run()\n  value = 1\n  value\nend", vec![]),
        ("def double(x)\n  x * 2\nend\n\ndouble(3)", vec![]),
        ("def run()\n  return 1\n  2\nend", vec!["3:3 (run)"]),
        (
            "def run()\n  if false\n    return 1\n  elsif true\n    return 2\n  else\n    return 3\n  end\n  4\nend",
            vec!["9:3 (run)"],
        ),
        (
            "def run()\n  begin\n    return 1\n  ensure\n    value = 2\n  end\n  3\nend",
            vec!["7:3 (run)"],
        ),
        (
            "def run()\n  begin\n    1\n  rescue\n    2\n  else\n    return 3\n    4\n  end\nend",
            vec!["8:5 (run)"],
        ),
        (
            "def run()\n  begin\n    1\n  rescue\n    return 2\n  else\n    return 3\n  end\n  4\nend",
            vec!["9:3 (run)"],
        ),
        (
            "class Reporter\n  def instance_path()\n    return 1\n    2\n  end\n\n  def self.class_path()\n    return 3\n    4\n  end\nend\n\ndef run()\n  Reporter.new.instance_path\nend",
            vec!["4:5 (Reporter#instance_path)", "9:5 (Reporter.class_path)"],
        ),
        (
            "def run()\n  [1].each do |x|\n    raise \"boom\"\n    x\n  end\nend",
            vec!["4:5 (run block at 2:12)"],
        ),
        (
            "def run()\n  %I[#{capture { raise \"boom\"; 1 }}]\nend",
            vec!["1:25 (run block at 1:9)"],
        ),
        (
            "class Reporter\n  raise \"boom\"\n  1\n\n  def value()\n    2\n  end\nend\n\ndef run()\n  Reporter.new.value\nend",
            vec!["3:3 (Reporter.<class body>)"],
        ),
        (
            "puts 1\nclass A\n  raise \"x\"\n  1\nend\nraise \"y\"\nz = 1 if true",
            vec!["4:3 (<script>)", "4:3 (A.<class body>)", "7:7 (<script>)"],
        ),
    ] {
        let path = files.write("script.vibe", source);
        let run = vibes(&["analyze", &path]);
        if findings.is_empty() {
            run.expect(0, "No issues found\n", "");
            continue;
        }
        let stdout: String = findings
            .iter()
            .map(|finding| {
                let (position, scope) = finding.split_once(' ').unwrap();
                format!("{path}:{position}: unreachable statement {scope}\n")
            })
            .collect();
        run.expect(
            1,
            &stdout,
            &format!("analysis found {} issue(s)\n", findings.len()),
        );
    }
}

#[test]
fn reports_usage_and_compile_errors() {
    vibes(&["analyze"]).fails("vibes analyze: script path required");
    vibes(&["analyze", "a", "b"]).fails("vibes analyze: expected a single script path");
    vibes(&["analyze", "-x"]).fails("flag provided but not defined: -x");
    let files = Files::new();
    let broken = files.write("broken.vibe", "def run(\n");
    vibes(&["analyze", &broken]).fails(
        "analysis compile failed: parse error at 2:1: expected name\n  --> line 2, column 1\n 2 | \n   | ^",
    );
    let missing = files.path("missing.vibe");
    vibes(&["analyze", &missing]).fails(&format!(
        "read script: open {missing}: no such file or directory"
    ));
    let oversized = files.write("oversized.vibe", &"#".repeat((1 << 20) + 1));
    vibes(&["analyze", &oversized]).fails(&format!(
        "read script: source exceeds maximum size ({} > 1048576 bytes)",
        (1 << 20) + 1
    ));
}
