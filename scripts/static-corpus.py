#!/usr/bin/env python3
"""Type check the migrated golden corpora with the static checker.

Every source `scripts/migrate-corpora.py` migrated without a manual
diagnostic must type check; this builds `examples/static_corpus.rs`, runs it
on `.cache/migrate/work/full` when that tree exists, prints the counts per
corpus and per diagnostic code, and compares the verdicts with the runtime
errors the goldens record (`scripts/static-agreement.py`).

    python3 scripts/static-corpus.py                  # every corpus
    python3 scripts/static-corpus.py --corpus replay  # some corpora
"""
import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / ".cache/migrate/work/full"
OUT = ROOT / ".cache/static/corpus.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--work", type=Path, default=WORK, help="the full migration's tree")
    parser.add_argument("--corpus", help="comma-separated corpora (default: every one)")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    if not args.work.exists():
        print(f"{args.work} does not exist; run scripts/migrate-corpora.py first", file=sys.stderr)
        return 0
    if not args.no_build:
        subprocess.run([ROOT / "scripts/cargo", "build", "--release", "--offline", "--example", "static_corpus"],
                       cwd=ROOT, check=True)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    command = [ROOT / "target/release/examples/static_corpus", args.work, "--out", OUT]
    if args.corpus:
        command += ["--corpus", args.corpus]
    checked = subprocess.run([str(part) for part in command], cwd=ROOT)
    subprocess.run([sys.executable, ROOT / "scripts/static-agreement.py", OUT], cwd=ROOT, check=True)
    return checked.returncode


if __name__ == "__main__":
    sys.exit(main())
