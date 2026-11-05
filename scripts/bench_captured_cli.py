#!/usr/bin/env python3
"""Compare preserved release CLIs on VIBE-169's captured loop and controls.

Build binaries separately, then pass --binary LABEL=PATH for each one. The
first binary is the baseline. This measures whole CLI processes, including
startup, compilation and output, rather than the library benchmark harness.
"""

import argparse
import hashlib
import json
import platform
import statistics
import subprocess
import time
from pathlib import Path


def fixtures():
    n = 2_000_000
    total = sum((i % 7) * 2 for i in range(n))
    captured = f"t = 0\n{n}.times {{ |i| t = t + (i % 7) * 2 }}\n"
    cases = {
        # The CLI prints the final times expression's result, its receiver.
        "captured": (captured, f"{n}\n"),
        "captured_output": (captured + "puts(t)\n", f"{total}\n"),
        "while": (
            f"t = 0\ni = 0\nwhile i < {n}\n"
            " t = t + (i % 7) * 2\n i += 1\nend\nputs(t)\n",
            f"{total}\n",
        ),
        "range": (
            f"t = 0\nfor i in 0...{n}\n"
            " t = t + (i % 7) * 2\nend\nputs(t)\n",
            f"{total}\n",
        ),
    }
    # The record-totaling workload from fixtures.py, repeated at CLI scale.
    rows = [
        {"price": 100 + i * 7, "qty": i % 5 + 1, "active": i % 7 != 0}
        for i in range(256)
    ]
    literal = "[" + ",".join(
        "{price: %d, qty: %d, active: %s}"
        % (row["price"], row["qty"], str(row["active"]).lower())
        for row in rows
    ) + "]"
    active = [row for row in rows if row["active"]]
    cases["record_totals"] = (
        "rows = " + literal + "\ntotal = 0\nunits = 0\n2000.times {\n"
        ' rows.each { |row|\n  next if !row["active"]\n'
        '  total += row["price"] * row["qty"]\n  units += row["qty"]\n'
        " }\n}\nputs(total)\nputs(units)\n",
        f'{2000 * sum(row["price"] * row["qty"] for row in active)}\n'
        f'{2000 * sum(row["qty"] for row in active)}\n',
    )
    return cases


def measure(binary, runner, source, expected):
    runner.unlink(missing_ok=True)
    runner.symlink_to(binary)
    start = time.perf_counter_ns()
    result = subprocess.run(
        [str(runner), "run", str(source)],
        capture_output=True, check=True, timeout=120,
    )
    elapsed = time.perf_counter_ns() - start
    if result.stdout != expected.encode() or result.stderr:
        raise RuntimeError(
            f"{binary}: {source.name}: expected {expected!r}, "
            f"got stdout={result.stdout!r}, stderr={result.stderr!r}"
        )
    return elapsed


def main():
    cases = fixtures()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--rounds", type=int, default=24)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--case", action="append", choices=cases)
    parser.add_argument("--out", type=Path, required=True, help="new output directory")
    args = parser.parse_args()
    if args.rounds < 1 or args.warmups < 1:
        parser.error("rounds and warmups must be positive")
    binaries = {}
    for item in args.binary:
        label, separator, path = item.partition("=")
        if not separator or not label or label in binaries:
            parser.error("each binary needs a unique nonempty LABEL=PATH")
        binary = Path(path).resolve()
        if not path or not binary.is_file():
            parser.error(f"binary does not exist: {path}")
        binaries[label] = binary
    if args.case:
        cases = {name: cases[name] for name in args.case}
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    samples = {case: {label: [] for label in binaries} for case in cases}
    metadata = {
        "platform": platform.platform(),
        "warmups": args.warmups,
        "rounds": args.rounds,
        "binaries": {
            label: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
            for label, path in binaries.items()
        },
        "fixtures": {name: {"source": source, "stdout": expected} for name, (source, expected) in cases.items()},
        "order": [],
        "samples_ns": samples,
    }
    runner = out / "vibes"
    try:
        for name, (source, expected) in cases.items():
            script = out / (name + ".vibe")
            script.write_text(source)
            for binary in binaries.values():
                for _ in range(args.warmups):
                    measure(binary, runner, script, expected)
        for round_index in range(args.rounds):
            labels = list(binaries)
            if (round_index // len(labels)) % 2:
                labels.reverse()
            offset = round_index % len(labels)
            labels = labels[offset:] + labels[:offset]
            metadata["order"].append(labels)
            for name, (_, expected) in cases.items():
                for label in labels:
                    elapsed = measure(binaries[label], runner, out / (name + ".vibe"), expected)
                    samples[name][label].append(elapsed)
            (out / "samples.json").write_text(json.dumps(metadata, indent=2) + "\n")
            print(f"round {round_index + 1}/{args.rounds}", flush=True)
    finally:
        runner.unlink(missing_ok=True)
    baseline = next(iter(binaries))
    summary = {}
    for name, variants in samples.items():
        summary[name] = {}
        reference = variants[baseline]
        for label, values in variants.items():
            median = statistics.median(values)
            summary[name][label] = {
                "median_ms": median / 1e6,
                "min_ms": min(values) / 1e6,
                "max_ms": max(values) / 1e6,
                "median_change_percent": 100 * (median / statistics.median(reference) - 1),
                "paired_change_percent": 100 * (
                    statistics.median(value / base for value, base in zip(values, reference)) - 1
                ),
            }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
