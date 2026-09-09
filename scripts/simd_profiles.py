#!/usr/bin/env python3
"""Select reviewed SIMD benchmark fixtures and validate their native results."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path, PurePosixPath
import re


CONTROLS = {
    "name": "controls",
    "benchmark": "BenchmarkString(Length|Index|RIndex|Slice)Loop(ASCII|Unicode)",
    "cases": 8,
}
PROFILE_DIRECTORY = "benchmarks/simd"


def repo_path(root, relative):
    """Return a repository path, rejecting paths that escape the checkout."""
    path = PurePosixPath(relative)
    if path.is_absolute() or ".." in path.parts or "\\" in relative:
        raise ValueError(f"invalid repository path: {relative}")
    candidate = root / path
    if not candidate.resolve().is_relative_to(root.resolve()):
        raise ValueError(f"repository path escapes checkout: {relative}")
    return candidate


def optional_bytes(path):
    return path.read_bytes() if path.is_file() else None


def source_snapshot(root, patterns):
    """Include file names and bytes so additions and deletions also count."""
    files = {}
    for pattern in patterns:
        repo_path(root, pattern)
        for path in root.glob(pattern):
            if path.is_file() and not path.name.endswith("_test.go"):
                relative = path.relative_to(root).as_posix()
                files[relative] = repo_path(root, relative).read_bytes()
    return files


def prepare(head, base):
    """Select changed profiles and copy only their fixtures to the PR base."""
    groups = [dict(CONTROLS)]
    for profile_path in sorted((head / PROFILE_DIRECTORY).glob("*.json")):
        profile_bytes = profile_path.read_bytes()
        profile = json.loads(profile_bytes)
        fixture = profile["fixture"]
        if not fixture.startswith("internal/runtime/") or not fixture.endswith("_benchmark_test.go"):
            raise ValueError(f"invalid benchmark fixture: {fixture}")
        fixture_path = repo_path(head, fixture)
        if type(profile["cases"]) is not int or profile["cases"] <= 0:
            raise ValueError(f"invalid case count: {profile_path}")
        re.compile(profile["benchmark"])
        for pattern in profile["sources"]:
            repo_path(head, pattern)
        if base is None or not fixture_path.is_file():
            continue
        fixture_bytes = fixture_path.read_bytes()
        relative_profile = profile_path.relative_to(head).as_posix()
        changed = (
            profile_bytes != optional_bytes(repo_path(base, relative_profile))
            or fixture_bytes != optional_bytes(repo_path(base, fixture))
            or source_snapshot(head, profile["sources"]) != source_snapshot(base, profile["sources"])
        )
        if changed:
            groups.append({
                "name": profile_path.stem,
                **profile,
                "fixture_sha256": hashlib.sha256(fixture_bytes).hexdigest(),
            })
    if base is not None:
        for group in groups[1:]:
            destination = repo_path(base, group["fixture"])
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(repo_path(head, group["fixture"]).read_bytes())
    return {
        "base": base is not None,
        "benchmark": "^(" + "|".join(group["benchmark"] for group in groups) + ")$",
        "cases": sum(group["cases"] for group in groups),
        "groups": groups,
    }


def validate(plan, results, avx2_disabled=False):
    """Require every selected case exactly six times in every build variant."""
    variants = ["head-nosimd", "head-simd"]
    if plan["base"]:
        variants += ["base-nosimd", "base-simd"]
    if avx2_disabled:
        variants.append("head-simd-avx2-disabled")
    expected_names = None
    for variant in variants:
        path = results / (variant + ".txt")
        counts = Counter()
        for line in path.read_text().splitlines():
            if not line.startswith("Benchmark"):
                continue
            match = re.match(r"^(Benchmark\S+)\s+\d+\s+\S+\s+ns/op(?:\s|$)", line)
            if match is None:
                raise ValueError(f"malformed benchmark result in {path}: {line}")
            name = re.sub(r"-\d+$", "", match[1])
            counts[name] += 1
        if len(counts) != plan["cases"] or any(count != 6 for count in counts.values()):
            raise ValueError(f"{path}: want {plan['cases']} unique cases with six samples each; got {dict(counts)}")
        group_counts = Counter()
        for name in counts:
            top_level = name.split("/", 1)[0]
            groups = [group for group in plan["groups"] if re.fullmatch(group["benchmark"], top_level)]
            if len(groups) != 1:
                raise ValueError(f"{path}: {name} must belong to exactly one selected profile")
            group_counts[groups[0]["name"]] += 1
        for group in plan["groups"]:
            if group_counts[group["name"]] != group["cases"]:
                raise ValueError(f"{path}: incorrect case count for {group['name']}")
        names = set(counts)
        if expected_names is not None and names != expected_names:
            raise ValueError(f"{path}: benchmark case names differ between build variants")
        expected_names = names


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    select = commands.add_parser("prepare")
    select.add_argument("--head", type=Path, required=True)
    select.add_argument("--base", type=Path)
    select.add_argument("--output", type=Path, required=True)
    pattern = commands.add_parser("pattern")
    pattern.add_argument("plan", type=Path)
    check = commands.add_parser("validate")
    check.add_argument("plan", type=Path)
    check.add_argument("results", type=Path)
    check.add_argument("--avx2-disabled", action="store_true")
    args = parser.parse_args()
    if args.command == "prepare":
        plan = prepare(args.head, args.base)
        args.output.write_text(json.dumps(plan, indent=2) + "\n")
        print(f"Selected {plan['cases']} cases: " + ", ".join(group["name"] for group in plan["groups"]))
    elif args.command == "pattern":
        print(json.loads(args.plan.read_text())["benchmark"])
    else:
        validate(json.loads(args.plan.read_text()), args.results, args.avx2_disabled)
        print("Every selected benchmark case has six samples in every build variant.")


if __name__ == "__main__":
    main()
