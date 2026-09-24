#!/usr/bin/env python3
"""Run the Go reference CLI and this CLI on the same inputs and compare them.

Builds the Go v0.70.0 `vibes` command from .cache/reference-go-0.70.0 (named
`vibes`, since its help text uses the program name) unless --go names one,
and uses target/debug/vibes unless --rust names another binary. It compares
exit status, stdout and stderr for the help and error paths, `fmt` over every
`.vibe` file in the repository and the reference plus generated whitespace
cases (stdout, -check and -w), `run` and `analyze` over the script corpus, and
`test` over a generated suite. Differences are printed; see docs/cli.md for
the intentional ones. The `cli` golden corpus checks this CLI without Go.
"""
import argparse
import json
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REFERENCE = ROOT / ".cache/reference-go-0.70.0"

# Argument lists for the help, dispatch and flag-error paths.
CASES = [
    [], ["--help"], ["-h"], ["-h", "-x"], ["--help", "run"], ["help"], ["h"],
    ["help", "run"], ["help", "fmt"], ["help", "analyze"], ["help", "test"],
    ["help", "lsp"], ["help", "repl"], ["help", "help"], ["help", "h"],
    ["help", "nope"], ["help", ""], ["help", "run", "extra"], ["help", "run", "--"],
    ["help", "--", "run"], ["help", "-h", "run"], ["help", "-x"], ["h", "--help"],
    ["run", "-h"], ["fmt", "--help"], ["analyze", "--help"], ["test", "--help"],
    ["lsp", "--help"], ["repl", "--help"], ["unknown"], ["unknown", "--help"],
    ["unknown", "", "--help"], ["--", "run"], ["-help"], ["--h"], ["-h="],
    ["--help=false"], [""], [" run"], ["RUN"], ["-"], ["-x"], ["--bogus"], ["---x"],
    ["-=x"], ["-x=1"], ["-1"], ["-1a"], ["--1"], ["-é"], ["-_"], ["-version"],
    ["run", "-unknown"], ["fmt", "--unknown=value"], ["run", "--e"], ["fmt", "---w"],
    ["run", "--=value"], ["run", "-e="], ["run", "-e", "   "], ["run"], ["run", "-watch"],
    ["run", "-module-path"], ["run", "-module-path", "/nonexistent", "-e", "1"],
    ["run", "/nonexistent.vibe"], ["run", "-e", "1", "-watch"], ["run", "-e", "1", "extra"],
    ["run", "-function", "f", "-e", "1"], ["run", "-check=yes", "-e", "1"],
    ["run", "-step-quota=nope", "-unknown"], ["run", "-step-quota", "99999999999999999999"],
    ["run", "-profile", "nope", "-e", "1"], ["run", "--help=1"], ["run", "-x", "-h"],
    ["run", "-e", "1 + 2"], ["run", "-e", "[1, nil, 2.0, 1e20, :s, {a: [nil]}]"],
    ["run", "-check", "-e", "1"],
    ["check"], ["check", "a.vibe", "b.vibe"], ["check", "/nonexistent.vibe"],
    ["fmt"], ["fmt", "-"], ["fmt", "/nonexistent"], ["analyze"], ["analyze", "-x"],
    ["analyze", "a", "b"], ["test", "-run"], ["test", "/nonexistent"],
    ["test", "-profile", "nope"], ["lsp", "extra"], ["repl", "extra"],
    ["repl", "-profile", "nope"],
]


def build_go(out):
    binary = out / "vibes"
    subprocess.run([ROOT / "scripts/go", "build", "-o", binary, "./cmd/vibes"], cwd=REFERENCE, check=True)
    return binary


def execute(binary, args, cwd):
    try:
        done = subprocess.run([str(binary), *args], cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
        return done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""


class Report:
    def __init__(self, go, rust):
        self.go, self.rust, self.failed = go, rust, 0

    def compare(self, label, runs, verbose=True):
        same = 0
        for args, cwd in runs:
            go, rust = execute(self.go, args, cwd), execute(self.rust, args, cwd)
            if go == rust:
                same += 1
            elif verbose:
                print(f"--- {label}: {json.dumps(args)}")
                for name, a, b in zip(["status", "stdout", "stderr"], go, rust):
                    if a != b:
                        print(f"  {name}\n    go:   {a!r}\n    rust: {b!r}")
        self.failed += len(runs) - same
        print(f"{label}: {same}/{len(runs)} identical", flush=True)


def corpus():
    files = set(REFERENCE.glob("examples/**/*.vibe")) | set(REFERENCE.glob("tests/**/*.vibe"))
    files |= set(ROOT.glob("tests/site/**/*.vibe")) | set(ROOT.glob("tests/upstream/**/*.vibe"))
    return sorted(files | set(ROOT.glob("examples/*.vibe")))


def formatting_tree(base):
    rng = random.Random(3)
    for index, path in enumerate(corpus() + sorted(REFERENCE.glob("cmd/vibes/testdata/**/*.vibe"))):
        target = base / f"c{index // 50}" / f"f{index}.vibe"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(path.read_bytes())
    pieces = [" ", "\t", "\r", "\n", "\r\n", "  \t", "x", "#", "é", "\n \n"]
    for index in range(2000):
        text = "".join(rng.choice(pieces) for _ in range(rng.randint(0, 30)))
        target = base / f"r{index // 100}" / f"r{index}.vibe"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(text.encode().replace(b"#", b"\xff", 1))


def test_suite(base):
    files = {
        "math_test.vibe": "def test_addition()\n  assert 1 + 2 == 3\nend\n\ndef helper()\n  1\nend\n",
        "broken_test.vibe": "def test_failure()\n  assert 1 == 2, \"one is not two\"\nend\n\ndef test_needs(value)\nend\n\ndef test_prints\n  puts \"hello\"\n  raise \"boom\"\nend\n",
        "top_test.vibe": "puts \"top\"\ndef test_a\nend\n",
        "assign_test.vibe": "x = 1\n",
        "empty_test.vibe": "def helper\nend\n",
        "nested/deep_test.vibe": "def test_div\n  1 / 0\nend\n",
    }
    for name, text in files.items():
        (base / name).parent.mkdir(parents=True, exist_ok=True)
        (base / name).write_text(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--go", type=Path, help="a Go reference binary named vibes")
    parser.add_argument("--rust", type=Path, default=ROOT / "target/debug/vibes")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="compare-cli-", dir=ROOT / ".cache") as scratch:
        scratch = Path(scratch)
        go = args.go or build_go(scratch)
        report = Report(go, args.rust.resolve())
        report.compare("help and errors", [(case, scratch) for case in CASES])
        tree = scratch / "fmt"
        formatting_tree(tree)
        report.compare("fmt", [(["fmt", "fmt"], scratch), (["fmt", "-check", "fmt"], scratch)])
        for name in ["go-w", "rust-w"]:
            shutil.copytree(tree, scratch / name)
        go_w = execute(go, ["fmt", "-w", "go-w"], scratch)
        rust_w = execute(report.rust, ["fmt", "-w", "rust-w"], scratch)
        trees_match = subprocess.run(["diff", "-r", "go-w", "rust-w"], cwd=scratch, capture_output=True).returncode == 0
        same = go_w[0] == rust_w[0] and go_w[2] == rust_w[2] and trees_match
        print(f"fmt -w: {'identical' if same else 'DIFFERENT'} trees and output")
        report.failed += 0 if same else 1
        scripts = corpus()
        report.compare("run", [(["run", str(path)], path.parent) for path in scripts])
        report.compare("analyze", [(["analyze", str(path)], path.parent) for path in scripts])
        suite = scratch / "suite"
        test_suite(suite)
        report.compare("test", [(["test"], suite), (["test", "-run", "fail|add", "."], suite), (["test", "empty_test.vibe"], suite)])
    print("differences:", report.failed)
    sys.exit(1 if report.failed else 0)


if __name__ == "__main__":
    main()
