#!/usr/bin/env python3
"""Compare recovery with an immutable pre-change golden harness.

Every corpus mutation keeps its acceptance and its complete first diagnostic.
Crashes, per-case timeouts, duplicate diagnostics and unbounded output fail the
sweep. Every machine-applicable V0003 fix is also applied and checked: the
fixed source must no longer report that V0003, nor a syntax error at the fix
that the mutation did not already have. Raw cases and observations go only to
--out.
"""
import argparse
import json
import random
from pathlib import Path

import golden
from mutations import recovery_mutations, suffix_mutations


def cases():
    result = golden.parse_cases()
    rng = random.Random(0x700)
    # A separate generator keeps the other mutations as they were.
    suffixes = random.Random(0x3003)
    for path in sorted(p for root in golden.PARSE_SOURCES
                       for p in (golden.ROOT / root).rglob("*.vibe")):
        text = golden.read_source(path)
        for kind, source in recovery_mutations(text, rng):
            result.append({"id": f"{path.relative_to(golden.ROOT)}:{kind}", "source": source})
        for kind, source in suffix_mutations(text, suffixes):
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


NAME_SUFFIX = "V0003"


def shift(edits, position):
    """Where `position` of a source lies once `edits` apply to it."""
    moved = position
    for (start, end), replacement in edits:
        if end <= position and (start, end) != (position, position):
            moved += len(replacement.encode()) - (end - start)
        elif start < position:
            return None
    return moved


def apply(source, edits):
    data = source.encode()
    for (start, end), replacement in sorted(edits, reverse=True):
        data = data[:start] + replacement.encode() + data[end:]
    return data.decode()


def suffix_fixes(inputs, after):
    """Each machine-applicable V0003 fix as a case of its fixed source."""
    fixed = []
    for case in inputs:
        record = after[case["id"]]
        for index, diagnostic in enumerate(record.get("diagnostics", [])):
            if diagnostic.get("code") != NAME_SUFFIX:
                continue
            for fix in diagnostic.get("fixes", []):
                if fix.get("applicability") != "Always":
                    continue
                edits = [(tuple(edit["span"]), edit["replacement"]) for edit in fix["edits"]]
                fixed.append({
                    "id": f"{case['id']}:fix{index}",
                    "source": apply(case["source"], edits),
                    "parse": True,
                    "_origin": (case["id"], diagnostic, edits),
                })
    return fixed


def fix_failures(fixed, unfixed, results):
    """The fixes that leave their V0003 or add a syntax error where they edit."""
    failures = []
    for case in fixed:
        origin, diagnostic, edits = case["_origin"]
        at = diagnostic["span"][0]
        # The edit at the diagnostic, which may start earlier in its run.
        (start, end), replacement = next(
            edit for edit in edits if edit[0][0] <= at < max(edit[0][1], edit[0][0] + 1))
        moved = shift([edit for edit in edits if edit[0][1] <= start], start)
        spot = (moved, moved + len(replacement.encode()))
        existing = set()
        for old in unfixed[origin].get("diagnostics", []):
            position = shift(edits, old["span"][0]) if "span" in old else None
            if position is not None:
                existing.add((old["code"], position, old["message"]))
        for new in results[case["id"]].get("diagnostics", []):
            if "span" not in new or not spot[0] <= new["span"][0] <= spot[1]:
                continue
            if new["code"] == NAME_SUFFIX:
                failures.append((case["id"], "the fix leaves a V0003 where it edits"))
            elif new["code"] == "V0001" and \
                    (new["code"], new["span"][0], new["message"]) not in existing:
                failures.append((case["id"], f"the fix adds V0001: {new['message']}"))
        if results[case["id"]].get("phase") in ("panic", "crash", "hang"):
            failures.append((case["id"], results[case["id"]]))
    return failures


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
    fixed = suffix_fixes(inputs, after)
    fixes = golden.run_engine(args.harness.resolve(), fixed, args.jobs, "fixes") if fixed else {}
    failures.extend(fix_failures(fixed, after, fixes))
    print(f"fixes: {len(fixed)} applied", flush=True)
    summary = {"cases": len(inputs), "recovered": recovered,
               "maximum_errors": maximum, "suffix_fixes": len(fixed), "failures": failures}
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k != "failures"}), flush=True)
    if failures:
        print(f"{len(failures)} failures; see {args.out / 'summary.json'}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
