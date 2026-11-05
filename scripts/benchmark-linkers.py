#!/usr/bin/env python3
"""Compare linkers without changing Rust flags or rebuilding dependencies."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent


def capture(command: list[str]) -> str:
    """Capture diagnostic output, including unsuccessful version probes."""
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    return (result.stdout + result.stderr).strip()


def link_driver(arguments: list[str]) -> int:
    """Time the compiler driver's selected linker and append one atomic record."""
    variant = os.environ["VIBESCRIPT_BENCH_VARIANT"]
    arguments = [arg for arg in arguments if not arg.startswith("-fuse-ld=")]
    selected = os.environ["VIBESCRIPT_BENCH_" + variant.upper()]
    command = [os.environ["VIBESCRIPT_BENCH_CC"], *arguments]
    if selected:
        command.append("-fuse-ld=" + selected)
    started = time.perf_counter()
    result = subprocess.run(command, check=False)
    elapsed = time.perf_counter() - started
    output = arguments[arguments.index("-o") + 1] if "-o" in arguments else None
    record = {
        "sample": os.environ["VIBESCRIPT_BENCH_SAMPLE"],
        "variant": variant,
        "seconds": elapsed,
        "returncode": result.returncode,
        "output": output,
        "arguments_sha256": hashlib.sha256(json.dumps(arguments).encode()).hexdigest(),
    }
    descriptor = os.open(os.environ["VIBESCRIPT_BENCH_EVENTS"], os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(descriptor, (json.dumps(record) + "\n").encode())
    finally:
        os.close(descriptor)
    return result.returncode


def validate(executable: str, workload: str, log: Path, environment: dict[str, str]) -> dict[str, object]:
    """Execute the produced integration suite or verify CLI evaluation."""
    command = [executable, "--test-threads=3"] if workload == "integration" else [executable, "-e", "6 * 7"]
    result = subprocess.run(command, cwd=ROOT, env=environment, text=True, capture_output=True, check=False)
    log.write_text(result.stdout + result.stderr)
    success = result.returncode == 0 and (workload == "integration" or result.stdout.strip() == "42")
    return {"passed": success, "returncode": result.returncode, "output_tail": (result.stdout + result.stderr)[-500:]}


def summarize(output: Path, samples: list[dict[str, object]], metadata: dict[str, object]) -> None:
    """Persist raw evidence and a concise table of successful measured builds."""
    summary = []
    for profile in ("dev", "gate"):
        for workload in ("cli", "integration"):
            for variant in ("baseline", "mold"):
                group = [sample for sample in samples if sample["profile"] == profile and sample["workload"] == workload and sample["variant"] == variant and not sample["warmup"] and sample["returncode"] == 0]
                if not group:
                    continue
                summary.append({
                    "profile": profile, "workload": workload, "variant": variant, "n": len(group),
                    "rebuild_median_s": statistics.median(float(sample["rebuild_s"]) for sample in group),
                    "link_median_s": statistics.median(float(sample["link_s"]) for sample in group),
                    "rebuild_min_s": min(float(sample["rebuild_s"]) for sample in group),
                    "rebuild_max_s": max(float(sample["rebuild_s"]) for sample in group),
                    "link_min_s": min(float(sample["link_s"]) for sample in group),
                    "link_max_s": max(float(sample["link_s"]) for sample in group),
                })
    (output / "results.json").write_text(json.dumps({"metadata": metadata, "summary": summary, "samples": samples}, indent=2) + "\n")
    lines = ["# Linker benchmark", "", f"Platform: {metadata['platform']}", f"Source: `{metadata['source_revision']}`", f"Mold: `{metadata['mold_revision']}`", "", "Warm rebuilds after touching only the target's root source, with unchanged contents. Dependencies are cached. Each pair alternates order; warmups and validation are excluded. Link time includes the compiler driver and excludes the Python wrapper startup. Rebuild time includes Cargo, rustc and linking. All features are enabled; builds use three Cargo jobs.", "", "| Profile | Target | Linker | Samples | Link median (s) | Rebuild median (s) |", "|---|---|---|---:|---:|---:|"]
    for row in summary:
        lines.append(f"| {row['profile']} | {row['workload']} | {row['variant']} | {row['n']} | {row['link_median_s']:.4f} | {row['rebuild_median_s']:.4f} |")
    failures = [sample for sample in samples if sample["returncode"] != 0 or sample.get("validation", {}).get("passed") is False]
    if failures:
        lines.extend(["", "Failures:", *[f"- {sample['id']}: {sample.get('error', sample.get('validation'))}" for sample in failures]])
    (output / "REPORT.md").write_text("\n".join(lines) + "\n")


def main() -> int:
    """Warm each workload, alternate linkers, and validate both outputs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mold", type=Path, required=True)
    parser.add_argument("--mold-revision", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=5)
    args = parser.parse_args()
    if args.rounds < 2:
        parser.error("--rounds must be at least 2")
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=True)
    events_path = output / "link-events.jsonl"
    if events_path.exists():
        parser.error("use a fresh output directory")
    host = capture(["rustc", "-vV"]).split("host: ")[1].splitlines()[0]
    linker_dir = output / "linkers"
    linker_dir.mkdir()
    mold = linker_dir / ("ld64.mold" if sys.platform == "darwin" else "ld.mold")
    mold.symlink_to(args.mold.resolve())
    baseline = ""
    if sys.platform == "linux":
        rust_lld = Path(capture(["rustc", "--print", "sysroot"])) / "lib/rustlib" / host / "bin/rust-lld"
        if not rust_lld.is_file():
            parser.error(f"cannot find the toolchain's bundled LLD: {rust_lld}")
        baseline_link = linker_dir / "ld.lld"
        baseline_link.symlink_to(rust_lld)
        baseline = str(baseline_link)
    elif sys.platform != "darwin":
        parser.error("supported hosts are macOS and Linux")
    driver = output / "driver"
    driver.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(Path(__file__).resolve()))} --link-driver \"$@\"\n")
    driver.chmod(0o755)
    environment = dict(os.environ)
    environment.update({
        "CARGO_BUILD_JOBS": "3", "CARGO_TERM_COLOR": "never", "TZ": "America/Detroit",
        "CARGO_TARGET_" + host.upper().replace("-", "_") + "_LINKER": str(driver),
        "VIBESCRIPT_BENCH_CC": shutil.which("clang") or "clang",
        "VIBESCRIPT_BENCH_BASELINE": baseline,
        "VIBESCRIPT_BENCH_MOLD": str(mold),
        "VIBESCRIPT_BENCH_EVENTS": str(events_path),
    })
    metadata = {
        "platform": platform.platform(), "host": host,
        "cpu": capture(["sysctl", "-n", "machdep.cpu.brand_string"]) if sys.platform == "darwin" else capture(["lscpu"]),
        "rustc": capture(["rustc", "-vV"]), "clang": capture([environment["VIBESCRIPT_BENCH_CC"], "--version"]),
        "baseline": capture(["ld", "-v"]) if sys.platform == "darwin" else capture([baseline, "--version"]),
        "mold_version": capture([str(mold), "-v" if sys.platform == "darwin" else "--version"]), "mold_revision": args.mold_revision,
        "source_revision": capture(["git", "-C", str(ROOT), "rev-parse", "HEAD"]),
        "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
        "python": sys.version, "rounds": args.rounds, "cargo_jobs": 3, "timezone": environment["TZ"],
        "rustflags": environment.get("RUSTFLAGS", ""),
    }
    samples: list[dict[str, object]] = []
    for profile in ("dev", "gate"):
        for workload, source in (("cli", ROOT / "cli/src/main.rs"), ("integration", ROOT / "tests/all.rs")):
            command = [str(ROOT / "scripts/cargo"), "build" if workload == "cli" else "test", "--locked", "--offline", "--all-features", "--profile", profile, "--message-format=json-render-diagnostics"]
            command.extend(["-p", "vibes", "--bin", "vibes"] if workload == "cli" else ["-p", "vibescript", "--test", "all", "--no-run"])
            disabled: set[str] = set()
            original_times = (source.stat().st_atime_ns, source.stat().st_mtime_ns)
            try:
                for round_index in range(args.rounds + 1):
                    order = ("baseline", "mold") if round_index % 2 == 0 else ("mold", "baseline")
                    for variant in order:
                        if variant in disabled:
                            continue
                        identifier = f"{profile}-{workload}-{variant}-{round_index}"
                        print(f"Starting {identifier}", flush=True)
                        environment.update({"VIBESCRIPT_BENCH_VARIANT": variant, "VIBESCRIPT_BENCH_SAMPLE": identifier})
                        source.touch()
                        started = time.perf_counter()
                        result = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, text=True, check=False)
                        elapsed = time.perf_counter() - started
                        (output / f"{identifier}.log").write_text(result.stdout + result.stderr)
                        links = [json.loads(line) for line in events_path.read_text().splitlines()] if events_path.exists() else []
                        links = [link for link in links if link["sample"] == identifier and link["output"]]
                        sample = {"id": identifier, "profile": profile, "workload": workload, "variant": variant, "warmup": round_index == 0, "returncode": result.returncode, "rebuild_s": elapsed, "link_s": sum(link["seconds"] for link in links), "links": links, "command": command}
                        if result.returncode != 0:
                            sample["error"] = result.stderr[-1800:]
                            disabled.add(variant)
                            print(f"FAILED {identifier}: {result.stderr[-1000:]}", flush=True)
                        elif round_index > 0 and len(links) != 1:
                            raise RuntimeError(f"{identifier}: expected exactly one relink, got {len(links)}")
                        if result.returncode == 0 and round_index == args.rounds:
                            artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
                            executables = [artifact["executable"] for artifact in artifacts if artifact.get("reason") == "compiler-artifact" and artifact.get("executable")]
                            sample["validation"] = validate(executables[-1], workload, output / f"{identifier}-validation.log", environment)
                        samples.append(sample)
                        summarize(output, samples, metadata)
                        print(f"Finished {identifier}: rebuild={elapsed:.3f}s link={sample['link_s']:.3f}s", flush=True)
            finally:
                os.utime(source, ns=original_times)
    print((output / "REPORT.md").read_text(), flush=True)
    return int(any(sample["returncode"] != 0 or sample.get("validation", {}).get("passed") is False for sample in samples))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--link-driver":
        sys.exit(link_driver(sys.argv[2:]))
    sys.exit(main())
