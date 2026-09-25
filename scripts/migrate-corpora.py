#!/usr/bin/env python3
"""Migrate the golden corpora with `vibes migrate` and check them against their goldens.

Each migratable corpus is exported with its recorded invocations
(`golden.py --export`), migrated in place with those invocations as
observation inputs, and run with `golden.py --sources`. The full migration
repairs each source with the static checker's diagnostics until it type
checks or no repair helps, running the source's invocations to keep what
they do (`--no-repair` skips that). The summary counts, per corpus, the
cases whose sources migrated without any diagnostic, the cases that need
manual work grouped by diagnostic code, and the cases whose observable
behaviour changed, which must be none. With `--static` it also type checks
the migrated sources and counts those that check clean, among the automatic
sources and among all of them.

    python3 scripts/migrate-corpora.py --compatible   # only rewrites today's runtime accepts
    python3 scripts/migrate-corpora.py                # the full migration, for the new compiler
    python3 scripts/migrate-corpora.py --static       # and how much of it type checks
"""
import argparse
import collections
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import golden  # noqa: E402

ROOT = golden.ROOT
VIBES = ROOT / "target/release/vibes"
# Problems that are not behaviour changes: moved error positions and accounting.
BENIGN = {"position drift in migrated sources", "quota outcomes that followed accounting drift"}


def run(command, **kwargs):
    print("+ " + " ".join(str(part) for part in command), flush=True)
    return subprocess.run([str(part) for part in command], check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus", help="comma-separated corpora (default: every migratable one)")
    parser.add_argument("--compatible", action="store_true", help="pass --compatible to vibes migrate")
    parser.add_argument("--no-repair", action="store_true", help="pass --no-repair to vibes migrate")
    parser.add_argument("--static", action="store_true",
                        help="type check the migrated sources and count those that check clean")
    parser.add_argument("--work", type=Path, default=ROOT / ".cache/migrate/work",
                        help="where exports, migrated trees and reports go")
    parser.add_argument("--no-build", action="store_true", help="use the built vibes and golden harness")
    parser.add_argument("--reexport", action="store_true", help="export the corpora again")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    args = parser.parse_args()
    names = args.corpus.split(",") if args.corpus else [c.name for c in golden.CORPORA.values() if c.migratable]
    corpora = [golden.CORPORA[name] for name in names]
    if not args.no_build:
        run([golden.CARGO, "build", "--release", "--locked", "-p", "vibes"], cwd=ROOT)
        if args.static:
            run([golden.CARGO, "build", "--release", "--locked", "--example", "static_corpus"], cwd=ROOT)
    export = args.work / "export"
    if args.reexport or not all((export / name / "index.json").exists() for name in names):
        shutil.rmtree(export, ignore_errors=True)
        run([sys.executable, ROOT / "scripts/golden.py", "--export", export, "--corpus", ",".join(names)])
    mode = "compatible" if args.compatible else "full"
    tree = args.work / mode
    tree.mkdir(parents=True, exist_ok=True)
    reports = {}
    for name in names:
        shutil.rmtree(tree / name, ignore_errors=True)
        shutil.copytree(export / name, tree / name)
        command = [VIBES, "migrate", "--write", "--report", "json", "--inputs", tree / name / "inputs.jsonl"]
        if args.compatible:
            command.append("--compatible")
        if args.no_repair:
            command.append("--no-repair")
        report = tree / f"{name}.report.json"
        with open(report, "w") as out:
            run(command + [tree / name], stdout=out)
        reports[name] = {entry["file"]: entry for entry in json.loads(report.read_text())}
    failures = tree / "failures.json"
    check = [sys.executable, ROOT / "scripts/golden.py", "--corpus", ",".join(names), "--sources", tree,
             "--failures", failures, "--jobs", args.jobs]
    if args.no_build:
        check.append("--no-build")
    subprocess.run([str(part) for part in check])
    problems = json.loads(failures.read_text())
    summary = {}
    for corpus in corpora:
        index = json.loads((tree / corpus.name / "index.json").read_text())
        files = reports[corpus.name]
        changed = set()
        for category, entry in problems.get(corpus.name, {}).items():
            if category != "total" and category not in BENIGN:
                changed.update(entry["cases"])
        automatic, manual, unchanged = 0, collections.Counter(), 0
        cases = corpus.cases()
        for case in cases:
            codes, touched = set(), False
            for key, _ in golden.source_keys(case):
                entry = files.get(index[key], {})
                touched |= entry.get("changed", False)
                codes.update(diagnostic["code"] for diagnostic in entry.get("diagnostics", []))
            if codes:
                for code in codes:
                    manual[code] += 1
            else:
                automatic += 1
            unchanged += not touched
        summary[corpus.name] = {
            "cases": len(cases),
            "automatic": automatic,
            "manual": sum(1 for case in cases if any(
                files.get(index[key], {}).get("diagnostics") for key, _ in golden.source_keys(case))),
            "manual_by_reason": dict(manual.most_common()),
            "unchanged": unchanged,
            "behaviour_changes": len(changed),
            "behaviour_change_examples": sorted(changed)[:20],
        }
    if args.static:
        for scope, flags in (("automatic", []), ("all", ["--all"])):
            out = tree / f"static-{scope}.json"
            subprocess.run([str(ROOT / "target/release/examples/static_corpus"), str(tree), "--out", str(out),
                            "--corpus", ",".join(names), *flags], stdout=subprocess.DEVNULL)
            checked = json.loads(out.read_text())["summary"]
            for name in names:
                if name in checked:
                    summary[name][f"static_{scope}"] = {
                        "sources": checked[name]["automatic_sources"],
                        "clean": checked[name]["clean_sources"],
                    }
    (tree / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print()
    print(f"{'corpus':14} {'cases':>8} {'automatic':>10} {'manual':>8} {'changed':>8}"
          + (f" {'clean/automatic':>18} {'clean/all':>16}" if args.static else ""))
    for name, entry in summary.items():
        clean = ""
        if args.static:
            clean = " ".join(f"{entry[key]['clean']:>8}/{entry[key]['sources']:<{width}}"
                             for key, width in (("static_automatic", 9), ("static_all", 7)))
        print(f"{name:14} {entry['cases']:>8} {entry['automatic']:>10} {entry['manual']:>8} "
              f"{entry['behaviour_changes']:>8} {clean}")
        for code, count in entry["manual_by_reason"].items():
            print(f"{'':14}   {code}: {count}")
    return 1 if any(entry["behaviour_changes"] for entry in summary.values()) else 0


if __name__ == "__main__":
    sys.exit(main())
