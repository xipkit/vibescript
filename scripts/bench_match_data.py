#!/usr/bin/env python3
"""Paired match-data measurements using separate timing and allocator binaries."""

import argparse
import hashlib
import json
import platform
import re
import statistics
import subprocess
from pathlib import Path

from fixtures import benchmark_cases


def workloads():
    original = next(c for c in benchmark_cases() if c["name"] == "regex_log_lines/metered")
    original = {**original, "name": "regex_log_lines"}
    pattern = r"user=([a-z]+) id=([0-9]+) status=([a-z]+)"
    lines = original["args"][0]
    matches = [re.search(pattern, line) for line in lines]
    cases = [original]

    def add(name, body, expected, result):
        cases.append(dict(name=name, source=f"def run(input: array<string>) -> {result}\n{body}\nend\n",
                          args=[lines], expected=expected))

    def mapped(name, value, expected, result):
        add(name, f"input.map {{ |line| m=line.match(/{pattern}/); {value} }}", expected, result)

    mapped("unread", "m == nil", [m is None for m in matches], "array<bool>")
    for index in (0, 1):
        mapped(f"index{index}", f"m == nil ? nil : m[{index}]",
               [m[index] if m else None for m in matches], "array<string?>")
    mapped("repeat_index", 'm == nil ? nil : m[1].as(string)+m[1].as(string)',
           [m[1] * 2 if m else None for m in matches], "array<string?>")
    mapped("offsets", "m == nil ? [] : [m.begin(0),m.end(0),m.begin(1),m.end(1)]",
           [[m.start(), m.end(), m.start(1), m.end(1)] if m else [] for m in matches],
           "array<array<int?>>")
    add("whole_store", f"rows=input.map {{ |line| line.match(/{pattern}/) }}\n"
        "rows.map { |m| m == nil ? nil : m[1] }",
        [m[1] if m else None for m in matches], "array<string?>")
    add("unread_groupless", "input.map { |line| line.match(/" + pattern.replace("(", "").replace(")", "")
        + "/) == nil }", [m is None for m in matches], "array<bool>")
    add("predicate", f"input.map {{ |line| line.match?(/{pattern}/) }}",
        [m is not None for m in matches], "array<bool>")
    add("named", "input.map { |line| m=line.match(/user=(?<user>[a-z]+) id=(?<id>[0-9]+) status=(?<status>[a-z]+)/);"
        " m == nil ? nil : m.named_captures }",
        [dict(zip(("user", "id", "status"), m.groups())) if m else None for m in matches],
        "array<hash<string, string?>?>")
    long = dict(name="long_subject", source="def run(input: string) -> array<string?>\n"
                "input.match(/(a)/).as(match_data).captures\nend\n",
                args=["x" * 500_000 + "a" + "x" * 500_000], expected=["a"])
    cases.append(long)
    return [{**case, "name": case["name"] + ("_unlimited" if not metered else ""),
             "accounting": metered} for metered in (True, False) for case in cases]


def run(binary, fixture, iterations, mode):
    result = subprocess.run([str(binary), str(fixture), str(iterations), mode],
                            check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "baseline-alloc", "candidate-alloc"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=8)
    parser.add_argument("--minimum-ms", type=int, default=150)
    parser.add_argument("--case", action="append", help="measure only these workload names")
    args = parser.parse_args()
    if args.rounds < 2 or args.minimum_ms < 1:
        parser.error("need at least two rounds and a positive minimum duration")
    cases = workloads()
    if args.case:
        unknown = set(args.case) - {case["name"] for case in cases}
        if unknown:
            parser.error(f"unknown workloads: {sorted(unknown)}")
        cases = [case for case in cases if case["name"] in args.case]
    args.out.mkdir(parents=True, exist_ok=False)
    binaries = {name: getattr(args, name.replace("-", "_")).resolve()
                for name in ("baseline", "candidate", "baseline-alloc", "candidate-alloc")}
    report = {"platform": platform.platform(), "machine": platform.machine(),
              "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
              "runtime_sha256": hashlib.sha256(Path("src/regex/matches.rs").read_bytes()).hexdigest(),
              "rounds": args.rounds, "minimum_ms": args.minimum_ms,
              "binaries": {name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                           for name, path in binaries.items()}, "cases": []}
    for case in cases:
        fixture = args.out / (case["name"] + ".json")
        fixture.write_text(json.dumps([case]) + "\n")
        record = {"name": case["name"], "source": case["source"],
                  "fixture_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
                  "measurements": {}}
        for label, binary in binaries.items():
            validation = run(binary, fixture, 1, "validate")
            if json.dumps(json.loads(validation["result_json"]), sort_keys=True) != json.dumps(case["expected"], sort_keys=True):
                raise RuntimeError(f"{case['name']}: {label} produced an unexpected value")
            record["measurements"][label] = {"validation": validation}
            if label.endswith("-alloc"):
                record["measurements"][label]["allocations"] = run(binary, fixture, 100, "alloc")
        calibration = [run(binaries[label], fixture, 50, "timing")["ns_per_call"]
                       for label in ("baseline", "candidate")]
        iterations = max(1, int(args.minimum_ms * 1_000_000 / min(calibration)) + 1)
        record["iterations"] = iterations
        for label in ("baseline", "candidate"):
            record["measurements"][label]["rounds"] = []
        for round_number in range(args.rounds):
            order = ("baseline", "candidate") if round_number % 2 == 0 else ("candidate", "baseline")
            for label in order:
                record["measurements"][label]["rounds"].append(run(binaries[label], fixture, iterations, "timing"))
        for label in ("baseline", "candidate"):
            samples = [r["ns_per_call"] for r in record["measurements"][label]["rounds"]]
            record["measurements"][label].update(median_ns=statistics.median(samples),
                                                min_ns=min(samples), max_ns=max(samples))
        report["cases"].append(record)
        (args.out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
        print(case["name"], {label: record["measurements"][label]["median_ns"]
                              for label in ("baseline", "candidate")}, flush=True)


if __name__ == "__main__":
    main()
