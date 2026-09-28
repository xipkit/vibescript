#!/usr/bin/env python3
"""Compare recovery with an immutable pre-change golden harness.

Every corpus mutation keeps its acceptance and its complete first diagnostic.
Crashes, per-case timeouts, duplicate diagnostics and unbounded output fail the
sweep. Raw cases and observations go only to --out.
"""
import argparse
import json
import random
from pathlib import Path

import golden
from mutations import recovery_mutations


def cases():
    result = golden.parse_cases()
    rng = random.Random(0x700)
    for path in sorted(p for root in golden.PARSE_SOURCES
                       for p in (golden.ROOT / root).rglob("*.vibe")):
        for kind, source in recovery_mutations(golden.read_source(path), rng):
            result.append({"id": f"{path.relative_to(golden.ROOT)}:{kind}", "source": source})
    for name, source in {
        "closers": ")] }\n" * 10000,
        "openers": "([{" * 10000,
        "deep-blocks": "if true\n" * 2000 + "end\n" * 2000,
        "many-errors": "value = )\n" * 10000,
        "many-names": "".join(f"v{i} = {i}\n" for i in range(10000)) + "x = )\ny = ]\n",
        "long-expression": "1+" * 10000 + ")\nnext = ]\n",
        "literals": "\"#{)}\"\n/[/zz\n0x_\n" * 1000,
    }.items():
        result.append({"id": f"adversarial:{name}", "source": source})
    for case in result:
        case["parse"] = True
    return result


def first(record):
    record = dict(record)
    if record.get("diagnostics"):
        record["diagnostics"] = [{k: v for k, v in record["diagnostics"][0].items()
                                  if k not in ("span", "fixes")}]
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--harness", type=Path, default=golden.HARNESS)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    golden.SCRATCH = args.out / "scratch"
    golden.HANG_SECONDS = 10
    inputs = cases()
    golden.write_jsonl(args.out / "inputs.jsonl.gz", inputs)
    observations = []
    for name, harness in (("before", args.baseline), ("after", args.harness)):
        records = golden.run_engine(harness.resolve(), inputs, args.jobs, name)
        golden.write_jsonl(args.out / f"{name}.jsonl.gz", list(records.values()))
        observations.append(records)
        print(f"{name}: {len(records)} cases", flush=True)
    before, after = observations
    failures = []
    recovered = 0
    maximum = 0
    for case in inputs:
        cid = case["id"]
        old, new = before[cid], after[cid]
        diagnostics = new.get("diagnostics", [])
        maximum = max(maximum, len(diagnostics))
        recovered += len(diagnostics) > 1
        if new.get("phase") in ("panic", "crash", "hang"):
            failures.append((cid, new))
        if first(old) != first(new):
            failures.append((cid, "first error or acceptance changed"))
        keys = [golden.canonical(d) for d in diagnostics]
        if len(keys) != len(set(keys)) or len(keys) > min(100, len(case["source"].encode()) + 1):
            failures.append((cid, "duplicate or excessive diagnostics"))
        size = len(case["source"].encode())
        for diagnostic in diagnostics:
            if "span" in diagnostic:
                start, end = diagnostic["span"]
                if not 0 <= start <= end <= size:
                    failures.append((cid, "invalid diagnostic span"))
    summary = {"cases": len(inputs), "recovered": recovered,
               "maximum_errors": maximum, "failures": failures}
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k != "failures"}), flush=True)
    if failures:
        print(f"{len(failures)} failures; see {args.out / 'summary.json'}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
