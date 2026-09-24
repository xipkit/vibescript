#!/usr/bin/env python3
"""Compare the first compile error of the Go reference CLI and this CLI on
mutated copies of the site, upstream and reference example programs.

Each program yields copies with a token deleted, a token duplicated, a stray
`)`, `]`, `}`, `=`, `,`, `end`, `do`, `|`, `:` or `.` inserted before a token,
and every prefix of whole lines. Tokens are sampled with a fixed seed. Both
binaries run `vibes check` (or `--command run`) on every copy, and the first
`compile failed:` line of each is compared; a copy one side accepts and the
other rejects is an acceptance difference. Differences are summarized by
message shape and written to --out. The `parse` golden corpus records this
parser's results on the same kind of copies without Go.
"""
import argparse
import collections
import hashlib
import json
import random
import re
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ["tests/site", "tests/upstream", ".cache/reference-go-0.70.0/examples"]
STRAYS = [")", "]", "}", "=", ",", "end", "do", "|", ":", "."]
TOKEN = re.compile(r'''
    (?P<comment>\#[^\n]*)
  | (?P<ws>[ \t\r]+)
  | (?P<nl>\n)
  | (?P<str>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')
  | (?P<num>\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)
  | (?P<word>@{0,2}[A-Za-z_\u0080-￿][\w\u0080-￿]*[?!]?)
  | (?P<op>\*\*=|\|\|=|&&=|<=>|===|\.\.\.|\*\*|==|!=|<=|>=|&&|\|\||\+=|-=|\*=|/=|%=|<<|::|->|=>|=~|!~|&\.|\.\.)
  | (?P<ch>.)
''', re.X | re.S)


def tokens(source):
    """Returns the (start, end) span of every token outside comments and spaces."""
    return [(m.start(), m.end()) for m in TOKEN.finditer(source) if m.lastgroup not in ("ws", "nl", "comment")]


def mutations(source, rng, per_kind):
    """Yields (kind, text) for each distinct mutation of source."""
    spans = tokens(source)
    seen = set()

    def emit(kind, text):
        if text not in seen and text != source:
            seen.add(text)
            yield kind, text

    if spans:
        def picks():
            return rng.sample(range(len(spans)), min(per_kind, len(spans)))
        for i in picks():
            start, end = spans[i]
            yield from emit("delete", source[:start] + source[end:])
        for i in picks():
            start, end = spans[i]
            yield from emit("duplicate", source[:end] + source[start:end] + source[end:])
        for stray in STRAYS:
            for i in picks():
                start, _ = spans[i]
                gap = " " if stray.isalpha() else ""
                yield from emit("insert " + stray, source[:start] + stray + gap + source[start:])
    lines = source.split("\n")
    for n in range(1, len(lines)):
        yield from emit("truncate", "\n".join(lines[:n]))


def first_error(binary, path, command):
    """Returns the first `compile failed:` message, None when compilation succeeds."""
    try:
        result = subprocess.run([binary, command, path], capture_output=True, text=True, timeout=20, errors="replace")
    except subprocess.TimeoutExpired:
        return "TIMEOUT"
    for line in (result.stdout + result.stderr).splitlines():
        if line.startswith("compile failed: "):
            return line[len("compile failed: "):].strip()
    return None


def shape(message):
    """Reduces a message to its wording without position or quoted text."""
    message = re.sub(r"^parse error at \d+:\d+: ", "", message or "")
    message = re.sub(r'"[^"]*"', '"X"', message)
    return re.sub(r"'[^']*'", "'X'", message)[:90]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--go", default=str(ROOT / ".cache/cli-commands/go/vibes"), help="Go reference vibes binary")
    parser.add_argument("--rust", default=str(ROOT / "target/release/vibes"), help="Rust vibes binary")
    parser.add_argument("--command", default="check", choices=["check", "run"])
    parser.add_argument("--per-kind", type=int, default=8, help="tokens sampled per mutation kind")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--out", type=Path, help="write the differences here as JSON")
    args = parser.parse_args()

    rng = random.Random(args.seed)
    cases, seen = [], set()
    for path in sorted(p for d in SOURCES for p in (ROOT / d).rglob("*.vibe")):
        source = path.read_text(errors="replace")
        for kind, text in mutations(source, rng, args.per_kind):
            digest = hashlib.sha1(text.encode()).digest()
            if digest not in seen:
                seen.add(digest)
                cases.append((str(path.relative_to(ROOT)), kind, text))

    with tempfile.TemporaryDirectory(prefix="parse-sweep") as scratch:
        def run(item):
            index, (name, kind, text) = item
            path = Path(scratch) / f"{index}.vibe"
            path.write_text(text)
            go = first_error(args.go, str(path), args.command)
            rust = first_error(args.rust, str(path), args.command)
            path.unlink()
            return dict(file=name, kind=kind, source=text, go=go, rust=rust)

        with ThreadPoolExecutor(args.jobs) as pool:
            results = list(pool.map(run, enumerate(cases)))

    same = sum(r["go"] == r["rust"] for r in results)
    acceptance = [r for r in results if (r["go"] is None) != (r["rust"] is None)]
    messages = [r for r in results if r["go"] != r["rust"] and (r["go"] is None) == (r["rust"] is None)]
    print(f"{len(results)} programs: {same} identical, {len(acceptance)} acceptance differences, {len(messages)} message differences")
    for (go, rust), n in collections.Counter((shape(r["go"]), shape(r["rust"])) for r in messages).most_common(40):
        print(f"{n:6} go: {go}\n       rust: {rust}")
    for (side, message), n in collections.Counter(("go accepts" if r["go"] is None else "rust accepts", shape(r["go"] or r["rust"])) for r in acceptance).most_common(40):
        print(f"{n:6} {side}; other: {message}")
    if args.out:
        args.out.write_text(json.dumps(dict(total=len(results), same=same, acceptance=acceptance, differences=messages), indent=1) + "\n")


if __name__ == "__main__":
    main()
