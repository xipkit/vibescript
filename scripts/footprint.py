#!/usr/bin/env python3
"""Measure fresh-process startup, retained scripts, calls, and stripped CLI size."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(command, **kwargs):
    return subprocess.run([str(part) for part in command], cwd=ROOT, check=True, text=True, **kwargs)


def summarize(samples, stripped_size, paired=False):
    summary = {"rounds": len(samples["timing"]), "stripped_vibes_bytes": stripped_size,
               "timing_aggregation": "median of two-round means" if paired else "median", "stages": {}}
    for index, row in enumerate(samples["allocations"][0][:-1]):
        stage = summary["stages"][row["stage"]] = {}
        for key in sorted(row.keys() - {"stage"}):
            variant = "timing" if key in {"elapsed_ns", "rss_bytes"} else "allocations"
            values = [sample[index][key] for sample in samples[variant]]
            if key == "elapsed_ns" and paired:
                # Each two-round block places both binaries first and second.
                # A pooled median can fall between distinct cold-start modes.
                values = [(values[i] + values[i + 1]) / 2 for i in range(0, len(values), 2)]
                stage["elapsed_block_samples_ns"] = values
            stage[key] = None if any(value is None for value in values) else statistics.median(values)
        stage["elapsed_samples_ns"] = [sample[index]["elapsed_ns"] for sample in samples["timing"]]
        stage["rss_samples_bytes"] = [sample[index]["rss_bytes"] for sample in samples["timing"]]
    summary["workload"] = samples["allocations"][0][-1]
    assert all(sample[-1] == summary["workload"] for rounds in samples.values() for sample in rounds)
    return summary



def resummarize(out):
    paired = (out / "baseline").is_dir()
    for directory in ([out, out / "baseline"] if paired else [out]):
        samples = {}
        for variant in ["timing", "allocations"]:
            paths = sorted(directory.glob(f"{variant}-*.jsonl"), key=lambda path: int(path.stem.rsplit("-", 1)[1]))
            samples[variant] = [[json.loads(line) for line in path.read_text().splitlines()] for path in paths]
        if not samples["timing"] or len(samples["timing"]) != len(samples["allocations"]):
            raise ValueError(f"{directory}: incomplete timing/allocation samples")
        if paired and len(samples["timing"]) % 2:
            raise ValueError(f"{directory}: incomplete alternating pair")
        previous = json.loads((directory / "summary.json").read_text())
        summary = summarize(samples, previous["stripped_vibes_bytes"], paired=paired)
        (directory / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--out", type=Path)
    destination.add_argument("--summarize", type=Path, help="recompute summaries from saved per-process samples without building or running")
    parser.add_argument("--rounds", type=int, default=8)
    parser.add_argument("--check", action="store_true", help="enforce generous retained-heap and engine-allocation limits")
    parser.add_argument("--baseline", type=Path, help="prior footprint output with preserved timing and allocation binaries")
    args = parser.parse_args()
    if args.summarize:
        if args.baseline or args.check:
            parser.error("--summarize uses the baseline and samples already saved in the output")
        resummarize(args.summarize.resolve())
        return
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    if args.baseline and args.rounds % 2:
        parser.error("baseline comparisons require an even number of rounds")
    out = args.out.resolve()
    rustc = os.environ.get("RUSTC", "rustc")
    compiler = run([rustc, "-Vv"], capture_output=True).stdout
    flags = os.environ.get("RUSTFLAGS", "")
    samples = {}
    baseline_samples = {}
    baseline = args.baseline.resolve() if args.baseline else None
    if baseline:
        baseline_environment = json.loads((baseline / "environment.json").read_text())
        if baseline_environment["rustc"] != compiler:
            parser.error("baseline compiler differs; use the same Rust distribution and toolchain")
        if baseline_environment["RUSTFLAGS"] != flags:
            parser.error("baseline RUSTFLAGS differ")
        for variant in ["timing", "allocations"]:
            actual = hashlib.sha256((baseline / variant).read_bytes()).hexdigest()
            if actual != baseline_environment["binary_sha256"][variant]:
                parser.error(f"baseline {variant} binary differs from its recorded hash")
    out.mkdir(parents=True, exist_ok=False)
    if baseline:
        (out / "baseline").mkdir()
    # Timing and allocation instrumentation use separate release builds, just as
    # compare.py does. Every invocation starts with cold process-wide caches.
    with (out / "build.log").open("w") as log:
        for variant, features in [("timing", []), ("allocations", ["--features", "allocation-stats"])]:
            run([ROOT / "scripts/cargo", "build", "--offline", "--release", "--locked", "--example", "footprint", *features], stdout=log, stderr=log)
            binary = out / variant
            shutil.copy2(ROOT / "target/release/examples/footprint", binary)
            samples[variant] = []
            baseline_samples[variant] = []
            for index in range(args.rounds):
                order = [False, True] if baseline else [False]
                if index % 2:
                    order.reverse()
                for prior in order:
                    check_flags = ["--check"] if args.check and variant == "allocations" and not prior else []
                    result = run([baseline / variant if prior else binary, *check_flags], capture_output=True)
                    destination = out / "baseline" if prior else out
                    (destination / f"{variant}-{index:02}.jsonl").write_text(result.stdout)
                    (baseline_samples if prior else samples)[variant].append([json.loads(line) for line in result.stdout.splitlines()])
        run([ROOT / "scripts/cargo", "build", "--offline", "--release", "--locked", "-p", "vibes"], stdout=log, stderr=log)
    stripped = out / "vibes-stripped"
    shutil.copy2(ROOT / "target/release/vibes", stripped)
    run(["strip", stripped])
    environment = {
        "revision": run(["git", "rev-parse", "HEAD"], capture_output=True).stdout.strip(),
        "dirty": run(["git", "status", "--porcelain"], capture_output=True).stdout,
        "platform": platform.platform(), "machine": platform.machine(),
        "rustc": compiler,
        "rustc_sysroot": run([rustc, "--print", "sysroot"], capture_output=True).stdout.strip(),
        "cargo": run(["cargo", "-Vv"], capture_output=True).stdout,
        "RUSTFLAGS": flags,
        "command": sys.argv,
        "binary_sha256": {name: hashlib.sha256((out / name).read_bytes()).hexdigest() for name in ["timing", "allocations", "vibes-stripped"]},
    }
    (out / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    summary = summarize(samples, stripped.stat().st_size, paired=baseline is not None)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if baseline:
        baseline_summary = json.loads((baseline / "summary.json").read_text())
        previous = summarize(baseline_samples, baseline_summary["stripped_vibes_bytes"], paired=True)
        for key in ["calls", "source_bytes"]:
            assert previous["workload"][key] == summary["workload"][key], "baseline workload differs"
        baseline_environment["measurement_command"] = sys.argv
        (out / "baseline/environment.json").write_text(json.dumps(baseline_environment, indent=2) + "\n")
        (out / "baseline/summary.json").write_text(json.dumps(previous, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
