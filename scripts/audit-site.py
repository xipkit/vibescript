#!/usr/bin/env python3
"""Run every pinned site example and record compatibility and harness gaps."""
import argparse
import hashlib
import json
import subprocess
from collections import Counter
from pathlib import Path

from compare import BINS, ENV, ROOT, equal_json
from fixtures import SITE, site_cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    known = {c["name"].removeprefix("site/"): c["expected"] for c in site_cases()}
    manifest = json.loads((SITE / "sources.json").read_text())
    results = []
    for entry in manifest["files"]:
        name = entry["path"]
        case = {"name": name, "source": (SITE / name).read_text(), "function": "run", "args": [], "accounting": True}
        fixture = out / "input.json"
        fixture.write_text(json.dumps([case]) + "\n")
        record = {"path": name}
        for variant in ["go-simd", "rust-simd"]:
            try:
                proc = subprocess.run([str(BINS / variant), str(fixture), "1", "validate"], cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=10)
                result = {"exit_code": proc.returncode, "stderr": proc.stderr}
                if proc.returncode == 0:
                    result["output"] = json.loads(json.loads(proc.stdout)["result_json"])
            except subprocess.TimeoutExpired:
                result = {"exit_code": -1, "stderr": "process exceeded 10 seconds"}
            record[variant] = result
        go, rust = record["go-simd"], record["rust-simd"]
        if go["exit_code"]:
            record["status"] = "harness_gap"
        elif rust["exit_code"]:
            record["status"] = "rust_gap"
        elif equal_json(go["output"], rust["output"]):
            record["status"] = "match"
        else:
            record["status"] = "mismatch"
        if name in known and (record["status"] != "match" or not equal_json(rust["output"], known[name])):
            record["status"] = "regression"
        results.append(record)
    report = {
        "site_revision": manifest["revision"],
        "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True),
        "binary_sha256": {name: hashlib.sha256((BINS / name).read_bytes()).hexdigest() for name in ["go-simd", "rust-simd"]},
        "counts": dict(Counter(r["status"] for r in results)),
        "examples": results,
    }
    (out / "audit.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report["counts"]))
    if any(r["status"] in ["mismatch", "regression"] for r in results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
