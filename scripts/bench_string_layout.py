#!/usr/bin/env python3
"""Compare public string calls in ordinary executables with varied Go layouts."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


FIXTURES = ("simd_controls", "string_rindex")
CASES = 28


def digest(data):
    return hashlib.sha256(data).hexdigest()


def prepare_driver(head, destination):
    destination.mkdir()
    benchmarks = []
    hashes = {}
    for fixture in FIXTURES:
        source = head / "internal/runtime" / (fixture + "_benchmark_test.go")
        original = source.read_bytes()
        text = original.decode().replace("package runtime\n", "package main\n", 1)
        (destination / (fixture + ".go")).write_text(text)
        hashes[source.name] = digest(original)
        benchmarks.extend(re.findall(r"func (Benchmark\w+)\(b \*testing.B\)", text))
    main = '''package main

import (
    "regexp"
    "testing"

    "github.com/mgomes/vibescript/vibes"
    "github.com/mgomes/vibescript/vibes/value"
)

type Config = vibes.Config
type Engine = vibes.Engine
type Script = vibes.Script
type CallOptions = vibes.CallOptions
type Value = value.Value

const KindInt = value.KindInt

var MustNewEngine = vibes.MustNewEngine
var NewString = value.NewString
var NewInt = value.NewInt

func main() {
    testing.Main(regexp.MatchString, nil, []testing.InternalBenchmark{
'''
    main += "".join(f'        {{Name: "{name}", F: {name}}},\n' for name in benchmarks)
    main += "    }, nil)\n}\n"
    (destination / "main.go").write_text(main)
    (destination / "features_amd64.go").write_text('''//go:build goexperiment.simd

package main

import (
    "fmt"
    "simd/archsimd"
)

func init() { fmt.Printf("avx2: %t\\n", archsimd.X86.AVX2()) }
''')
    return hashes


def revision_info(root):
    files = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard"], cwd=root, text=True).splitlines()
    production = {}
    for name in sorted(set(files)):
        if (name.endswith(".go") and not name.endswith("_test.go")) or name in ("go.mod", "go.sum"):
            path = root / name
            if path.is_file():
                production[name] = digest(path.read_bytes())
    return {
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "production": production,
    }


def sample_names(sample):
    names = [line.split()[0] for line in sample.splitlines() if line.startswith("Benchmark") and "ns/op" in line]
    if len(names) != CASES or len(set(names)) != CASES:
        raise RuntimeError(f"expected {CASES} distinct cases, got {names}")
    return sorted(names)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, type=Path)
    parser.add_argument("--head", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--toolchain", default="go1.27.1")
    parser.add_argument("--experiments", default="nosimd,simd", help="comma-separated nosimd and/or simd")
    parser.add_argument("--seeds", default="0,1,2,3", help="comma-separated seeds; 0 is the ordinary linker layout")
    parser.add_argument("--count", type=int, default=6)
    parser.add_argument("--benchtime", default="100ms")
    args = parser.parse_args()
    try:
        seeds = [int(seed) for seed in args.seeds.split(",")]
    except ValueError:
        parser.error("seeds must be comma-separated integers")
    experiments = args.experiments.split(",")
    if args.count < 1 or not seeds or min(seeds) < 0 or len(seeds) != len(set(seeds)):
        parser.error("count must be positive and seeds must be distinct nonnegative integers")
    if len(experiments) != len(set(experiments)) or not set(experiments) <= {"nosimd", "simd"}:
        parser.error("experiments must be distinct nosimd and/or simd")
    roots = {"base": args.base.resolve(), "head": args.head.resolve()}
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, GOTOOLCHAIN=args.toolchain, GOMAXPROCS="1", GOAMD64="v1", GOWORK="off")
    for key in ("GODEBUG", "GOFLAGS", "VIBES_ESTIMATOR_VERIFY", "VIBES_ENV_RECYCLE_VERIFY", "VIBES_BUILTIN_CONTRACT_VERIFY"):
        env.pop(key, None)
    settings = json.loads(subprocess.check_output(["go", "env", "-json", "GOARCH", "GOOS", "GOHOSTARCH", "GOHOSTOS", "GOAMD64", "GOARM64", "CGO_ENABLED"], env=env, text=True))
    if (settings["GOARCH"], settings["GOOS"]) != (settings["GOHOSTARCH"], settings["GOHOSTOS"]):
        parser.error("this benchmark requires native executables")
    if settings["GOARCH"] not in ("arm64", "amd64"):
        parser.error("this benchmark supports arm64 and amd64")
    prefix = []
    if hasattr(os, "sched_getaffinity"):
        cpu = min(os.sched_getaffinity(0))
        if not shutil.which("taskset"):
            parser.error("taskset is required to pin the Linux benchmark CPU")
        prefix = ["taskset", "-c", str(cpu)]
        settings["affinity_cpu"] = cpu
    info = {name: revision_info(root) for name, root in roots.items()}
    hashes = prepare_driver(roots["head"], out / "driver")
    manifest = {"settings": settings, "toolchain": args.toolchain, "experiments": experiments, "revisions": info, "fixtures": hashes, "driver_sha256": digest(Path(__file__).read_bytes()), "seeds": seeds, "count": args.count, "benchtime": args.benchtime, "binaries": [], "samples": []}

    def save():
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    save()
    with (out / "environment.txt").open("w") as log:
        subprocess.run(["go", "version"], env=env, stdout=log, check=True)
        subprocess.run(["uname", "-a"], stdout=log, check=True)
        command = ["lscpu"] if settings["GOOS"] == "linux" else ["sysctl", "machdep.cpu.brand_string", "hw.ncpu"]
        subprocess.run(command, stdout=log, check=True)
    expected_names = None
    with tempfile.TemporaryDirectory(prefix="vibes-string-layout-", dir="/tmp") as temporary:
        temporary = Path(temporary)
        modules = {}
        for name, root in roots.items():
            module = temporary / name
            shutil.copytree(out / "driver", module)
            (module / "go.mod").write_text("module string-layout-benchmark\n\ngo 1.26\n\nrequire github.com/mgomes/vibescript v0.0.0\n\nreplace github.com/mgomes/vibescript => " + json.dumps(str(root)) + "\n")
            modules[name] = module
        for seed in seeds:
            binaries = {}
            for experiment in experiments:
                for name, module in modules.items():
                    binary = temporary / f"{name}-{experiment}"
                    cmd = ["go", "build", "-mod=mod", "-o", str(binary)]
                    if seed:
                        cmd.append(f"-ldflags=-randlayout={seed}")
                    cmd.append(".")
                    subprocess.run(cmd, cwd=module, env=dict(env, GOEXPERIMENT=experiment), check=True)
                    binaries[name, experiment] = binary
                    manifest["binaries"].append({"revision": name, "experiment": experiment, "seed": seed, "sha256": digest(binary.read_bytes())})
            modes = [(experiment, experiment, env) for experiment in experiments]
            if settings["GOARCH"] == "amd64" and "simd" in experiments:
                modes.append(("simd-avx2-disabled", "simd", dict(env, GODEBUG="cpu.avx2=off")))
            for trial in range(args.count):
                order = ("base", "head") if trial % 2 == 0 else ("head", "base")
                for mode, experiment, run_env in modes:
                    for name in order:
                        cmd = [*prefix, str(binaries[name, experiment]), "-test.run=^$", "-test.bench=.", "-test.benchmem", "-test.cpu=1", "-test.count=1", "-test.benchtime=" + args.benchtime]
                        sample = subprocess.check_output(cmd, cwd=modules[name], env=run_env, text=True)
                        if mode == "simd-avx2-disabled" and "avx2: false\n" not in sample:
                            raise RuntimeError("AVX2 was not disabled in the fallback executable")
                        names = sample_names(sample)
                        if expected_names is None:
                            expected_names = names
                        if names != expected_names:
                            raise RuntimeError("benchmark names changed between builds")
                        filename = f"{name}-{mode}-seed-{seed}.txt"
                        with (out / filename).open("a") as log:
                            log.write(sample)
                        manifest["samples"].append({"file": filename, "trial": trial, "cases": names})
                        save()
                print(f"Measured seed {seed}, round {trial + 1}/{args.count}", flush=True)
        for name, root in roots.items():
            if revision_info(root) != info[name]:
                raise RuntimeError(f"{name} production sources changed during measurement")
    manifest["complete"] = True
    save()


if __name__ == "__main__":
    main()
