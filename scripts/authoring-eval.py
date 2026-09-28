#!/usr/bin/env python3
"""Summarize the recorded authoring study or replay its frozen source files."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus/glue"
DATASET = CORPUS / "evaluation.jsonl"


def records():
    return [json.loads(line) for line in DATASET.read_text().splitlines()]


def summary(rows):
    for phase in ("before", "after"):
        attempts = [row for row in rows if row["event"] == "attempt" and row["phase"] == phase]
        first = [row for row in attempts if row["attempt"] == 1]
        passed = sum(row["passed"] for row in first)
        print(f"{phase}: {passed}/{len(first)} first try ({passed / len(first):.1%}); "
              f"{len(attempts) / len(first):.3f} mean attempts")
        probes = [row for row in rows if row["event"] == "repair_probe_attempt" and row["phase"] == phase]
        count = len({row["program"] for row in probes})
        direct = sum(row["assessment"]["direct"] for row in probes if row["attempt"] == 1)
        print(f"  seeded probes: {len(probes) / count:.2f} mean attempts; "
              f"{direct}/{count} diagnostic-directed repairs")
    frozen = [row for row in rows if row["event"] == "frozen_replay"]
    print(f"unchanged first drafts after fixes: {sum(row['passed'] for row in frozen)}/{len(frozen)}")


def replay(rows, binary, drafts, output):
    output.mkdir(parents=True, exist_ok=True)
    phase = "before" if drafts == "original" else "after"
    selected = [row for row in rows if row["event"] == "attempt"
                and row["phase"] == phase and row["attempt"] == 1]
    assert len(selected) == 40
    source_root = output / "sources"
    source_root.mkdir()
    passed = 0
    with (output / "replay.jsonl").open("x") as log:
        for row in selected:
            name = row["program"]
            directory = source_root / name
            directory.mkdir()
            for filename, source in row["sources"].items():
                assert Path(filename).name == filename
                assert hashlib.sha256(source.encode()).hexdigest() == row["files"][filename]
                (directory / filename).write_text(source)
            commands = []
            for args in [
                ["check", "--json", str(directory / f"{name}.vibe")],
                ["check", "--json", str(directory / f"{name}_test.vibe")],
                ["test", "--profile", "low", str(directory / f"{name}_test.vibe")],
            ]:
                result = subprocess.run([str(binary), *args], capture_output=True,
                                        text=True, timeout=30)
                commands.append(dict(args=args, status=result.returncode,
                                     stdout=result.stdout, stderr=result.stderr))
            ok = all(command["status"] == 0 for command in commands)
            passed += ok
            log.write(json.dumps(dict(program=name, drafts=drafts, passed=ok,
                                      commands=commands)) + "\n")
            print(f"{name}: {'PASS' if ok else 'FAIL'}")
    print(f"{passed}/40 passed; evidence: {output / 'replay.jsonl'}")
    return 0 if passed == 40 else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("summary")
    run = commands.add_parser("replay")
    run.add_argument("--bin", type=Path, required=True, help="the vibes binary to measure")
    run.add_argument("--drafts", choices=("original", "revised"), required=True)
    run.add_argument("--out", type=Path, required=True, help="a fresh directory on the large volume")
    args = parser.parse_args()
    rows = records()
    if args.command == "summary":
        summary(rows)
        return 0
    return replay(rows, args.bin.resolve(), args.drafts, args.out.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
