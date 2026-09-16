#!/usr/bin/env python3
"""Verify selected language semantics and keep unresolved reference differences visible."""
import argparse
import hashlib
import json
import re
import subprocess
from collections import Counter
from pathlib import Path

from compare import BINS, ENV, ROOT, VARIANTS, equal_json, invoke
from fixtures import host_global_cases
from module_fixtures import cases as module_cases, materialize
from capability_fixtures import cases as capability_cases
from block_fixtures import cases as block_cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cases = json.loads((ROOT / "docs/compatibility-cases.json").read_text())
    cases += [case for case in host_global_cases() if "policy" in case]
    cases += [case for case in module_cases() if "policy" in case]
    cases += [case for case in capability_cases() if "policy" in case]
    cases += [case for case in block_cases() if "policy" in case]
    fixtures = []
    for case in cases:
        fixture = {"name": case["name"], "source": case.get("source") or "def run(input)\n" + case["body"] + "\nend", "args": case.get("args", [None]), "accounting": True}
        for field in ("globals", "strict_effects", "files", "module_development", "module_allow", "module_deny", "allow_require", "capability_probe", "block_probe", "accounting"):
            if field in case:
                fixture[field] = case[field]
        fixtures.append(fixture)
    fixtures = materialize(fixtures, out)
    path = out / "inputs.json"
    errors = {c["name"] for c in cases if "expected_error" in c}
    path.write_text(json.dumps([f for f in fixtures if f["name"] not in errors]) + "\n")
    observed = {v: invoke(v, path, 1, "validate", out / f"{v}.jsonl") for v in VARIANTS}
    results = {v: {r["name"]: json.loads(r["result_json"]) for r in records} for v, records in observed.items()}
    accounting = [{r["name"]: tuple(r[key] for key in ("steps", "tracked_peak_bytes", "tracked_retained_bytes")) for r in observed[v]} for v in ("rust-portable", "rust-simd")]
    assert accounting[0] == accounting[1], "Rust accounting differs between portable and SIMD"
    for fixture in fixtures:
        if fixture["name"] not in errors:
            continue
        path = out / (fixture["name"] + ".json")
        path.write_text(json.dumps([fixture]) + "\n")
        for variant in VARIANTS:
            proc = subprocess.run([str(BINS / variant), str(path), "1", "validate"], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=10)
            (out / f"{fixture['name']}-{variant}.log").write_text(proc.stdout + proc.stderr)
            if proc.returncode == 0:
                result = json.loads(json.loads(proc.stdout)["result_json"])
            else:
                error = re.search(r"Error \{ kind: (\w+),", proc.stderr)
                assert variant.startswith("rust-") and proc.returncode == 1 and error, (variant, proc.returncode, proc.stderr)
                result = {"error_kind": error.group(1)}
            results[variant][fixture["name"]] = result
    records = []
    for case in cases:
        name = case["name"]
        values = {v: results[v][name] for v in VARIANTS}
        policy = case["policy"]
        assert policy in {"documented_value_semantics", "honor_regex_anchors", "protected_match_data", "stop_at_inclusive_endpoint", "return_break_value", "consistent_bindings", "catch_lookup_errors", "attached_capability_methods", "preserve_host_block_control", "unresolved"}, (name, policy)
        if any(not equal_json(values[v], case["go"]) for v in VARIANTS if v.startswith("go-")):
            status = "reference_changed"
        elif policy != "unresolved":
            expected = {"error_kind": case["expected_error"]} if "expected_error" in case else case["expected"]
            if not all(equal_json(values[v], expected) for v in VARIANTS if v.startswith("rust-")):
                status = "changed"
            else:
                status = "resolved" if all(equal_json(value, expected) for value in values.values()) else "intentional"
        elif all(equal_json(values[v], case["go"]) for v in VARIANTS):
            status = "resolved"
        elif all(equal_json(values[v], case["rust"]) for v in VARIANTS if v.startswith("rust-")):
            status = "open"
        else:
            status = "changed"
        records.append({"name": name, "status": status, "policy": policy, "reason": case["reason"], "results": values})
    counts = dict(Counter(r["status"] for r in records))
    report = {"counts": counts, "rust_accounting_equal": True, "binary_sha256": {v: hashlib.sha256((BINS / v).read_bytes()).hexdigest() for v in VARIANTS}, "cases": records}
    (out / "audit.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(counts))
    if any(r["status"] not in {"resolved", "intentional"} for r in records):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
