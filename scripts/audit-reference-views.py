#!/usr/bin/env python3
"""Characterize Go's copy oracle without treating it as the compatibility target."""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

from compare import BINS, GO, ROOT, equal_json, invoke


PROBE = r'''package runtime

import (
    "encoding/json"
    "os"
    "testing"
)

func TestPortViewOracle(t *testing.T) {
    path := os.Getenv("VIBES_PORT_CASES")
    data, err := os.ReadFile(path)
    if err != nil {
        t.Fatalf("read view cases %q: %v", path, err)
    }
    var cases []struct {
        Name string
        Body string
    }
    if err := json.Unmarshal(data, &cases); err != nil {
        t.Fatalf("decode view cases %q: %v", path, err)
    }
    for _, test := range cases {
        t.Run(test.Name, func(t *testing.T) {
            source := "def measured(input)\n" + test.Body + "\nend\ndef run()\nJSON.stringify(measured(nil))\nend"
            script := compileScriptDefault(t, source)
            result := callFunc(t, script, "run", nil)
            t.Logf("VIEW_ORACLE %s %s", test.Name, result.String())
        })
    }
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    module = json.loads(subprocess.check_output(
        [str(GO), "list", "-m", "-json", "github.com/mgomes/vibescript"],
        cwd=ROOT / "benchmarks/go", text=True))
    assert module["Version"] == "v0.70.0", module
    reference = out / "reference"
    # Go forbids overlay replacements inside its module cache.
    shutil.copytree(module["Dir"], reference)
    probe = out / "view_oracle_test.go"
    probe.write_text(PROBE)
    subprocess.run(["gofmt", "-w", str(probe)], check=True)
    overlay = out / "overlay.json"
    overlay.write_text(json.dumps({"Replace": {
        str(reference / "internal/runtime/port_view_oracle_test.go"): str(probe),
    }}) + "\n")
    binary = out / "go-views.test"
    with (out / "build.log").open("w") as log:
        subprocess.run([str(GO), "test", "-c", "-overlay", str(overlay),
                        "-o", str(binary), "./internal/runtime"],
                       cwd=reference, stdout=log, stderr=subprocess.STDOUT, check=True)
    cases = [c for c in json.loads((ROOT / "docs/compatibility-cases.json").read_text()) if "expected_error" not in c]
    inputs = out / "oracle-inputs.json"
    inputs.write_text(json.dumps(cases) + "\n")
    oracle = {}
    for mode, flag in [("normal", ""), ("always_copy", "1")]:
        env = {**os.environ, "VIBES_PORT_CASES": str(inputs), "VIBES_COW_ALWAYS_COPY": flag}
        process = subprocess.run([str(binary), "-test.run", "^TestPortViewOracle$", "-test.v"],
                                 cwd=reference, env=env, capture_output=True, text=True)
        (out / f"go-{mode}.log").write_text(process.stdout + process.stderr)
        process.check_returncode()
        values = {}
        for line in process.stdout.splitlines():
            if "VIEW_ORACLE " in line:
                name, encoded = line.split("VIEW_ORACLE ", 1)[1].split(" ", 1)
                values[name] = json.loads(encoded)
        assert len(values) == len(cases), (mode, len(values), len(cases))
        oracle[mode] = values
    inputs = out / "rust-inputs.json"
    fixtures = [{"name": c["name"], "source": "def run(input)\n" + c["body"] + "\nend",
                 "args": [None], "accounting": True} for c in cases]
    inputs.write_text(json.dumps(fixtures) + "\n")
    rust = {v: {r["name"]: json.loads(r["result_json"]) for r in invoke(
        v, inputs, 1, "validate", out / f"{v}.jsonl")} for v in ["rust-portable", "rust-simd"]}
    records = []
    for case in cases:
        name = case["name"]
        normal, copied = oracle["normal"][name], oracle["always_copy"][name]
        assert equal_json(normal, case["go"]), (name, "recorded Go result changed", normal)
        assert equal_json(rust["rust-portable"][name], rust["rust-simd"][name]), name
        value = rust["rust-simd"][name]
        records.append({"name": name, "go_normal": normal, "go_always_copy": copied,
                        "rust": value, "rust_matches_normal": equal_json(normal, value),
                        "rust_matches_copy": equal_json(copied, value)})
    counts = {"cases": len(records),
              "rust_matches_normal": sum(r["rust_matches_normal"] for r in records),
              "rust_matches_copy": sum(r["rust_matches_copy"] for r in records)}
    report = {"reference_module": module, "counts": counts, "cases": records,
              "probe_sha256": hashlib.sha256(probe.read_bytes()).hexdigest(),
              "binary_sha256": {"go-test": hashlib.sha256(binary.read_bytes()).hexdigest(),
                                **{v: hashlib.sha256((BINS / v).read_bytes()).hexdigest()
                                   for v in rust}}}
    (out / "audit.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(counts))


if __name__ == "__main__":
    main()
