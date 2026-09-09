"""Measure fixed production revisions across independent linker layouts."""

import hashlib
import json
import os
from pathlib import Path
import subprocess


root = Path.cwd()
base = root / "base"
head = root / "head"
out = Path(os.environ["RUNNER_TEMP"]) / "layout-results"
out.mkdir()
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOAMD64="v1", GOMAXPROCS="1")
manifest = {"revisions": {}, "seeds": list(range(13)), "benchtime": "100ms", "samples_per_seed": 1}
for revision, path in [("base", base), ("head", head)]:
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=path, text=True).strip()
    assert sha == os.environ[revision.upper() + "_SHA"]
    manifest["revisions"][revision] = sha

subprocess.run([
    "python3", "-B", str(head / "scripts/simd_profiles.py"), "prepare",
    "--head", str(head), "--base", str(base), "--output", str(out / "profiles.json"),
], check=True)
pattern = "^Benchmark(SIMDString.*|StringASCII(ShortCalls|MixedCalls)|JSONSpans.*|RegexpEscape.*|StringASCIICase|StringCaseComparison|Whitespace(Strip|Split))$"
manifest["pattern"] = pattern
manifest["affinity_cpu"] = min(os.sched_getaffinity(0))
with (out / "environment.txt").open("w") as log:
    for command in [["uname", "-a"], ["lscpu"], ["go", "version"], ["go", "env", "GOOS", "GOARCH", "GOAMD64", "GOTOOLCHAIN"]]:
        subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)

expected = None
for seed in manifest["seeds"]:
    modes = ["nosimd", "simd"] if seed % 2 == 0 else ["simd", "nosimd"]
    revisions = ["base", "head"] if seed % 2 == 0 else ["head", "base"]
    binaries = {}
    for mode in modes:
        for revision in revisions:
            path = root / revision
            binary = out / f"seed-{seed:02d}-{revision}-{mode}.test"
            command = ["go", "test", "-c", "-ldflags=-randlayout=" + str(seed), "-o", str(binary), "./internal/runtime"]
            subprocess.run(command, cwd=path, env=dict(env, GOEXPERIMENT=mode), check=True)
            binaries[revision, mode] = binary
            if seed == 0:
                with (out / f"{revision}-{mode}.asm").open("w") as log:
                    subprocess.run(["go", "tool", "objdump", "-s", "(stringRune|callStringMember|stringByteIndex|parseString|appendEscaped|regexpEscape)", str(binary)], env=env, stdout=log, check=True)
    for mode in modes:
        for revision in revisions:
            binary = binaries[revision, mode]
            destination = out / f"seed-{seed:02d}-{revision}-{mode}.txt"
            command = ["taskset", "-c", str(manifest["affinity_cpu"]), str(binary), "-test.run=^$", "-test.bench=" + pattern, "-test.benchmem", "-test.cpu=1", "-test.count=1", "-test.benchtime=" + manifest["benchtime"]]
            print(f"seed={seed} revision={revision} mode={mode}", flush=True)
            with destination.open("w") as log:
                subprocess.run(command, cwd=root / revision / "internal/runtime", env=dict(env, GOEXPERIMENT=mode), stdout=log, stderr=subprocess.STDOUT, check=True)
            names = []
            for line in destination.read_text().splitlines():
                if line.startswith("Benchmark") and "ns/op" in line:
                    names.append(line.split()[0])
            assert names and len(names) == len(set(names)), destination
            if expected is None:
                expected = set(names)
            assert set(names) == expected, (destination, set(names) ^ expected)
            manifest.setdefault("results", []).append({"seed": seed, "revision": revision, "mode": mode, "file": destination.name, "cases": len(names), "sha256": hashlib.sha256(destination.read_bytes()).hexdigest()})
            (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    for binary in binaries.values():
        binary.unlink()
print("Completed all independent layouts with matching fixtures and benchmark names.", flush=True)
