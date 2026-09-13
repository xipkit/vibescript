#!/usr/bin/env python3
"""Verify documented value semantics and keep unresolved reference differences visible."""
import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

from compare import BINS, ROOT, VARIANTS, equal_json, invoke


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cases = json.loads((ROOT / "docs/compatibility-cases.json").read_text())
    fixtures = [{"name": c["name"], "source": "def run(input)\n" + c["body"] + "\nend", "args": [None], "accounting": True} for c in cases]
    path = out / "inputs.json"
    path.write_text(json.dumps(fixtures) + "\n")
    results = {v: {r["name"]: json.loads(r["result_json"]) for r in invoke(v, path, 1, "validate", out / f"{v}.jsonl")} for v in VARIANTS}
    records = []
    for case in cases:
        name = case["name"]
        values = {v: results[v][name] for v in VARIANTS}
        policy = case["policy"]
        assert policy in {"documented_value_semantics", "unresolved"}, (name, policy)
        if any(not equal_json(values[v], case["go"]) for v in VARIANTS if v.startswith("go-")):
            status = "reference_changed"
        elif policy == "documented_value_semantics":
            status = "intentional" if all(equal_json(values[v], case["expected"]) for v in VARIANTS if v.startswith("rust-")) else "changed"
        elif all(equal_json(values[v], case["go"]) for v in VARIANTS):
            status = "resolved"
        elif all(equal_json(values[v], case["rust"]) for v in VARIANTS if v.startswith("rust-")):
            status = "open"
        else:
            status = "changed"
        records.append({"name": name, "status": status, "policy": policy, "reason": case["reason"], "results": values})
    counts = dict(Counter(r["status"] for r in records))
    report = {"counts": counts, "binary_sha256": {v: hashlib.sha256((BINS / v).read_bytes()).hexdigest() for v in VARIANTS}, "cases": records}
    (out / "audit.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(counts))
    if any(r["status"] not in {"resolved", "intentional"} for r in records):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
