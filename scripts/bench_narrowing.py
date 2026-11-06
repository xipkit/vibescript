#!/usr/bin/env python3
"""Compare preserved release binaries on the 32 checked-narrowing workloads."""

import argparse
import hashlib
import json
import math
import platform
import statistics
import subprocess
from pathlib import Path

from compare import equal_json, timing_reliability
from fixtures import benchmark_cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="NAME=PATH")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--rounds", type=int, default=8)
    args = parser.parse_args()
    binaries = {name: Path(path).resolve() for name, path in
                (value.split("=", 1) for value in args.binary)}
    if len(binaries) < 2 or args.rounds < 1 or args.rounds % len(binaries):
        parser.error("use at least two distinct binaries and a multiple of that many rounds")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    prefixes = ("numeric_loop/", "integer_power/", "big_integer_", "hash_lookup/",
                "range_each/", "loop_range/", "nested_blocks/", "regex_loop/")
    cases = [case for case in benchmark_cases() if case["name"].startswith(prefixes)]
    inputs = out / "inputs.json"
    inputs.write_text(json.dumps(cases) + "\n")
    runner = out / "runner"

    def run(name, count, mode):
        # Keep argv and its initial stack footprint identical for each binary.
        runner.unlink(missing_ok=True)
        runner.symlink_to(binaries[name])
        result = subprocess.run([runner, inputs, str(count), mode], check=True,
                                text=True, capture_output=True)
        return {record["name"]: record for record in map(json.loads, result.stdout.splitlines())}

    validation = {name: run(name, 1, "validate") for name in binaries}
    (out / "validation.json").write_text(json.dumps(validation, indent=2) + "\n")
    reference = next(iter(validation.values()))
    if len(reference) != len(cases):
        raise RuntimeError("validation did not return every case")
    for case in cases:
        if not equal_json(json.loads(reference[case["name"]]["result_json"]), case["expected"]):
            raise RuntimeError(f"{case['name']}: result differs from fixture expectation")
    for name, records in validation.items():
        if records != reference:
            raise RuntimeError(f"{name}: results, output or accounting differ")
    print(f"{len(cases)} cases: identical full validation records", flush=True)
    pilots = {name: run(name, 3, "timing") for name in binaries}
    for case in cases:
        ns = max(pilots[name][case["name"]]["ns_per_call"] for name in binaries)
        case["iterations"] = max(2, min(10000, math.ceil(50_000_000 / ns)))
    inputs.write_text(json.dumps(cases) + "\n")
    samples = {name: {case["name"]: [] for case in cases} for name in binaries}
    names = list(binaries)
    for turn in range(args.rounds):
        offset = turn % len(names)
        for name in names[offset:] + names[:offset]:
            records = run(name, 0, "timing")
            (out / f"{name}-{turn + 1}.json").write_text(json.dumps(records) + "\n")
            for key, record in records.items():
                if record["digest"] != reference[key]["digest"]:
                    raise RuntimeError(f"{name}/{key}: timed output differs")
                samples[name][key].append(record["ns_per_call"])
        print(f"round {turn + 1}/{args.rounds}", flush=True)
    summary = {
        "rounds": args.rounds,
        "platform": platform.platform(),
        "binaries": {name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                     for name, path in binaries.items()},
        "cases": {key: {name: {"median_ns": statistics.median(samples[name][key]),
                                "samples_ns": samples[name][key]}
                        for name in binaries} for key in reference},
    }
    summary["timing_reliability"] = timing_reliability(summary)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for key, row in summary["cases"].items():
        print(key, " ".join(f"{name}={value['median_ns']:.0f}" for name, value in row.items()))
    for name, report in summary["timing_reliability"].items():
        print(name, report)
    if not all(report["reliable"] for report in summary["timing_reliability"].values()):
        raise RuntimeError("unreliable timings; rerun on a quiet machine")


if __name__ == "__main__":
    main()
