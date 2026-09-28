#!/usr/bin/env python3
"""Pause the footprint example and record native allocator and resident mappings."""

import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
from pathlib import Path


def size(value):
    units = {"K": 1024, "M": 1024**2, "G": 1024**3}
    return round(float(value[:-1]) * units[value[-1]]) if value[-1] in units else int(value)


def macos(text, binary):
    totals = {"binary_text_rodata": 0, "binary_other": 0, "allocator": 0, "stacks": 0}
    regions = []
    for line in text.splitlines():
        match = re.match(r"^(.*?)\s+([0-9a-f]+)-([0-9a-f]+)\s+\[\s*(\S+)\s+(\S+)\s+(\S+)\s+(\S+)\] (.*)$", line)
        if not match:
            continue
        kind, start, end, virtual, resident, dirty, swapped, detail = match.groups()
        row = {"kind": kind, "start": start, "end": end, "virtual": size(virtual),
               "resident": size(resident), "dirty": size(dirty), "swapped": size(swapped), "detail": detail}
        regions.append(row)
        mapped = re.search(r"\s(/.*)$", detail)
        if mapped and Path(mapped.group(1)).resolve() == binary.resolve():
            group = "binary_text_rodata" if kind == "__TEXT" else "binary_other"
        elif kind.startswith("MALLOC"):
            group = "allocator"
        elif kind == "Stack":
            group = "stacks"
        else:
            continue
        totals[group] += row["resident"]
    return totals, regions


def linux(text, binary):
    totals = {"binary_text_rodata": 0, "binary_other": 0, "allocator": 0, "stacks": 0,
              "libraries_other": 0, "anonymous_other": 0}
    regions = []
    for line in text.splitlines():
        match = re.match(r"^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$", line)
        if match:
            start, end, permissions, path = match.groups()
            regions.append({"start": start, "end": end, "virtual": int(end, 16) - int(start, 16),
                            "permissions": permissions, "path": path})
        elif ":" in line and regions:
            key, value = line.split(":", 1)
            if value.strip().endswith(" kB"):
                regions[-1][key] = int(value.split()[0]) * 1024
    for index, row in enumerate(regions):
        path = row["path"]
        following = regions[index + 1] if index + 1 < len(regions) else None
        # glibc's secondary arena occupies the writable start of a 64 MiB
        # reservation. Keep other anonymous mappings separate from malloc.
        arena = (not path and row["permissions"].startswith("rw") and following
                 and not following["path"] and following["permissions"].startswith("---")
                 and row["end"] == following["start"]
                 and int(row["start"], 16) % (64 * 1024**2) == 0
                 and int(following["end"], 16) - int(row["start"], 16) == 64 * 1024**2)
        if path == str(binary):
            group = "binary_other" if "w" in row["permissions"] else "binary_text_rodata"
        elif path == "[stack]" or path.startswith("[stack:"):
            group = "stacks"
        elif path == "[heap]" or arena:
            group = "allocator"
        elif not path:
            group = "anonymous_other"
        else:
            group = "libraries_other"
        row["group"] = group
        totals[group] += row["Rss"]
    totals["smaps_rss"] = sum(row["Rss"] for row in regions)
    return totals, regions


def summarize_saved(output):
    environment = json.loads((output / "environment.json").read_text())
    binary = Path(environment["binary"])
    summary = {}
    for path in sorted(output.glob("*.vmmap")) + sorted(output.glob("*.smaps")):
        name = path.stem
        metadata = json.loads((output / (name + ".json")).read_text())
        parse = macos if path.suffix == ".vmmap" else linux
        totals, regions = parse(path.read_text(), binary)
        assert totals["binary_text_rodata"] > 0, "could not identify executable mapping"
        metadata["resident_categories_bytes"] = totals
        summary[name] = metadata
        (output / (name + ".json")).write_text(json.dumps(metadata, indent=2) + "\n")
        (output / (name + "-regions.json")).write_text(json.dumps(regions, indent=2) + "\n")
    assert summary, "no saved mappings"
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("output", type=Path, nargs="?")
    parser.add_argument("--trim", action="store_true", help="also measure native allocator pressure relief")
    parser.add_argument("--summarize", type=Path, help="rebuild summaries from saved native mappings")
    args = parser.parse_args()
    if args.summarize:
        if args.binary or args.output:
            parser.error("--summarize takes only a snapshot directory")
        summarize_saved(args.summarize)
        return
    if not args.binary or not args.output:
        parser.error("binary and output are required when collecting mappings")
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve()
    child = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True,
                             env={**os.environ, "VIBESCRIPT_FOOTPRINT_SNAPSHOT": "1"})
    summaries = {}
    try:
        for stage in ["compiled_100", "calls_1000"]:
            for trimmed in ([False, True] if args.trim else [False]):
                metadata = json.loads(child.stderr.readline())
                assert metadata["stage"] == stage and metadata["trimmed"] == trimmed, metadata
                metadata["pid"] = child.pid
                name = stage + ("-trimmed" if trimmed else "")
                if platform.system() == "Darwin":
                    result = subprocess.run(["vmmap", "-w", "-interleaved", "-noCoalesce", str(child.pid)],
                                            capture_output=True, text=True, check=True)
                    (args.output / (name + ".vmmap")).write_text(result.stdout)
                    (args.output / (name + ".vmmap.stderr")).write_text(result.stderr)
                    totals, regions = macos(result.stdout, binary)
                elif platform.system() == "Linux":
                    for source in ["smaps", "status", "maps"]:
                        content = Path(f"/proc/{child.pid}/{source}").read_text()
                        (args.output / (name + "." + source)).write_text(content)
                        if source == "smaps":
                            totals, regions = linux(content, binary)
                else:
                    parser.error("mapping collection requires macOS or Linux")
                assert totals["binary_text_rodata"] > 0, "could not identify executable mapping"
                metadata["resident_categories_bytes"] = totals
                summaries[name] = metadata
                (args.output / (name + ".json")).write_text(json.dumps(metadata, indent=2) + "\n")
                (args.output / (name + "-regions.json")).write_text(json.dumps(regions, indent=2) + "\n")
                child.stdin.write("t" if args.trim and not trimmed else "\n")
                child.stdin.flush()
        output, errors = child.communicate(timeout=120)
        assert child.returncode == 0, (child.returncode, errors)
        (args.output / "stages.jsonl").write_text(output)
        (args.output / "stderr.txt").write_text(errors)
        (args.output / "summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
        environment = {"binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                       "platform": platform.platform(), "trim": args.trim}
        (args.output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
    print(args.output)


if __name__ == "__main__":
    main()
