"""Measure the fallback workloads from issue 1291 in ordinary Go executables."""

from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess

from simd_embedding_probe import prepare


root = Path.cwd()
out = Path(os.environ["RUNNER_TEMP"]) / "x86-fallback-results"
out.mkdir()
revisions = {"base": os.environ["SIMD_BASE"], "head": os.environ["SIMD_HEAD"]}
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOAMD64="v1", GOMAXPROCS="1")
env.pop("GODEBUG", None)
cpu = min(os.sched_getaffinity(0))
full_matrix = os.environ.get("SIMD_FULL_MATRIX") == "true"
pattern = "^Benchmark.*$" if full_matrix else "^Benchmark(SIMDString.*|StringASCIICase|StringCaseComparison|JSONSpans.*)$"
manifest = {
    "revisions": revisions,
    "trees": {},
    "rounds": 0 if os.environ.get("SIMD_PROFILE_ONLY") == "true" else (6 if full_matrix else 10),
    "benchtime": "100ms",
    "affinity_cpu": cpu,
    "benchmark_pattern": pattern,
    "expected_cases": 231 if full_matrix else 116,
    "full_matrix": full_matrix,
    "binaries": [],
    "samples": [],
}
for revision, sha in revisions.items():
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root / revision, text=True).strip()
    assert actual == sha, (revision, actual, sha)
    manifest["trees"][revision] = subprocess.check_output(
        ["git", "rev-parse", "HEAD^{tree}"], cwd=root / revision, text=True
    ).strip()

prepare(root / "head", root / "head/cmd/simd-embedding-probe", full_matrix=full_matrix)
shutil.copytree(root / "head/cmd/simd-embedding-probe", root / "base/cmd/simd-embedding-probe")
shutil.copytree(root / "head/cmd/simd-embedding-probe", out / "driver")
with (out / "environment.txt").open("w") as log:
    for command in (["uname", "-a"], ["lscpu"], ["go", "version"]):
        subprocess.run(command, env=env, stdout=log, check=True)

binaries = Path(os.environ["RUNNER_TEMP"]) / "x86-fallback-binaries"
binaries.mkdir()
feature_source = out / "features.go"
feature_source.write_text(
    'package main\nimport ("fmt"; "simd/archsimd")\n'
    'func main() { fmt.Printf("%t\\n", archsimd.X86.AVX2()) }\n'
)
feature_binary = binaries / "features"
subprocess.run(
    ["go", "build", "-o", str(feature_binary), str(feature_source)],
    cwd=root / "head", env=dict(env, GOEXPERIMENT="simd"), check=True,
)
enabled = subprocess.check_output([str(feature_binary)], env=env, text=True).strip()
disabled = subprocess.check_output(
    [str(feature_binary)], env=dict(env, GODEBUG="cpu.avx2=off"), text=True
).strip()
assert enabled == "true" and disabled == "false", (enabled, disabled)
manifest["avx2"] = {"simd": enabled, "simd-avx2-disabled": disabled}

for revision in revisions:
    for mode in ("nosimd", "simd"):
        binary = binaries / f"{revision}-{mode}"
        print("Building", binary.name, flush=True)
        subprocess.run(
            ["go", "build", "-o", str(binary), "./cmd/simd-embedding-probe"],
            cwd=root / revision, env=dict(env, GOEXPERIMENT=mode), check=True,
        )
        manifest["binaries"].append({
            "revision": revision, "mode": mode, "bytes": binary.stat().st_size,
            "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        })
        with (out / f"{revision}-{mode}.asm").open("w") as log:
            subprocess.run(
                ["go", "tool", "objdump", "-s",
                 "(jsonValueParser.*parse(String|EscapedContents).*|unicodeDowncase|caseInsensitiveEqual|stringCase.*|asciiCase.*|stringRune.*|stringIsASCII.*)",
                 str(binary)], env=env, stdout=log, check=True,
            )

expected = None
for trial in range(manifest["rounds"]):
    order = list(revisions) if trial % 2 == 0 else list(reversed(revisions))
    modes = ["nosimd", "simd", "simd-avx2-disabled"]
    modes = modes[trial % 3:] + modes[:trial % 3]
    for mode in modes:
        experiment = "nosimd" if mode == "nosimd" else "simd"
        sample_env = dict(env, GOEXPERIMENT=experiment)
        if mode.endswith("avx2-disabled"):
            sample_env["GODEBUG"] = "cpu.avx2=off"
        for revision in order:
            print("Sample", trial, mode, revision, flush=True)
            command = [
                "taskset", "-c", str(cpu), str(binaries / f"{revision}-{experiment}"),
                "-test.run=^$", "-test.bench=" + pattern, "-test.cpu=1", "-test.count=1",
                "-test.benchtime=100ms", "-test.benchmem",
            ]
            sample = subprocess.check_output(
                command, cwd=root / revision / "internal/runtime", env=sample_env, text=True
            )
            names = [line.split()[0] for line in sample.splitlines()
                     if line.startswith("Benchmark") and "ns/op" in line]
            assert len(names) == manifest["expected_cases"] and len(set(names)) == len(names), names
            if expected is None:
                expected = set(names)
            assert set(names) == expected
            with (out / f"{revision}-{mode}.txt").open("a") as log:
                log.write(sample)
            manifest["samples"].append({
                "trial": trial, "revision": revision, "mode": mode, "cases": len(names),
            })
            (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

for revision in revisions:
    for workload in ("json", "index", "rindex"):
        profile = out / f"{revision}-{workload}.pprof"
        command = ["taskset", "-c", str(cpu), str(binaries / f"{revision}-simd")]
        with (out / f"{revision}-{workload}-profile.txt").open("w") as log:
            subprocess.run(
                command, cwd=root / revision / "internal/runtime",
                env=dict(env, GOEXPERIMENT="simd", SIMD_PROFILE_FILE=str(profile), SIMD_PROFILE_WORKLOAD=workload),
                stdout=log, check=True,
            )
        assert profile.stat().st_size > 0
        with (out / f"{revision}-{workload}-top.txt").open("w") as log:
            subprocess.run(
                ["go", "tool", "pprof", "-top", "-nodecount=50", str(binaries / f"{revision}-simd"), str(profile)],
                env=env, stdout=log, check=True,
            )
(out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print("Completed all", len(manifest["samples"]), "samples and CPU profiles.", flush=True)
