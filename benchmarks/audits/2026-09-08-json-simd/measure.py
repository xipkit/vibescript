"""Reproduce the JSON experiment with Go 1.27.1 on ARM64."""

import os
from pathlib import Path
import subprocess
import sys

here = Path(__file__).resolve().parent
root = here.parents[2]
out = Path(sys.argv[1]).resolve()
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOEXPERIMENT="simd")
subprocess.run([sys.executable, str(here / "prepare.py"), str(out / "overlays")], check=True)


def run(args, log=None, cwd=root, extra_env=None):
    settings = dict(env, **(extra_env or {}))
    if log is None:
        subprocess.run(args, cwd=cwd, env=settings, check=True)
    else:
        with (out / log).open("a") as stream:
            subprocess.run(args, cwd=cwd, env=settings, stdout=stream, stderr=subprocess.STDOUT, check=True)


variants = ["original", "scalar", "simd"]
for variant in variants:
    (out / f"bench-{variant}.txt").write_text("")
    run(["go", "test", "-c", "-overlay", str(out / f"overlays/{variant}.json"), "-o", str(out / f"runtime-{variant}.test"), "./internal/runtime"])
    run([str(out / f"runtime-{variant}.test"), "-test.run=^TestAuditJSON", "-test.count=1"], cwd=root / "internal/runtime")

for trial in range(10):
    for variant in variants[trial % 3:] + variants[:trial % 3]:
        run([str(out / f"runtime-{variant}.test"), "-test.run=^$", "-test.bench=^Benchmark(AuditJSON|ExecutionJSON(Parse|Stringify)Loop)$", "-test.benchmem", "-test.benchtime=100ms", "-test.count=1"], f"bench-{variant}.txt", root / "internal/runtime")
    print(f"Completed benchmark round {trial + 1}/10", flush=True)

for log in ("tests-simd.txt", "oracles-simd.txt", "fuzz-simd.txt", "vet-simd.txt"):
    (out / log).write_text("")
run(["go", "test", "-p", "2", "-overlay", str(out / "overlays/simd.json"), "./...", "-count=1"], "tests-simd.txt")
run(["go", "test", "-p", "2", "-overlay", str(out / "overlays/simd.json"), "./internal/runtime", "-timeout=20m", "-count=1"], "oracles-simd.txt", extra_env={"VIBES_ESTIMATOR_VERIFY": "1", "VIBES_ENV_RECYCLE_VERIFY": "1"})
run(["go", "test", "-overlay", str(out / "overlays/simd.json"), "./internal/runtime", "-run=^$", "-fuzz=^FuzzAuditJSONDifferential$", "-fuzztime=30s", "-parallel=4"], "fuzz-simd.txt")
run(["go", "vet", "-p", "2", "-overlay", str(out / "overlays/simd.json"), "./..."], "vet-simd.txt")
print(f"Measurements and checks saved in {out}")
