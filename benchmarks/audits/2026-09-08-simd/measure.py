"""Measure the three ASCII implementations on Go 1.27.1, darwin/arm64."""

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


variants = ["scalar", "word", "simd"]
for variant in variants:
    (out / f"end-to-end-{variant}.txt").write_text("")
    run(["go", "test", "-c", "-overlay", str(out / f"overlays/{variant}.json"), "-o", str(out / f"runtime-{variant}.test"), "./internal/runtime"])

(out / "kernels.txt").write_text("")
run([str(out / "runtime-scalar.test"), "-test.run=^TestAuditASCIIParity$", "-test.bench=^BenchmarkAuditASCII$", "-test.benchmem", "-test.benchtime=100ms", "-test.count=6"], "kernels.txt", root / "internal/runtime")
for trial in range(6):
    for variant in variants[trial % 3:] + variants[:trial % 3]:
        run([str(out / f"runtime-{variant}.test"), "-test.run=^$", "-test.bench=^BenchmarkString(Length|Index|RIndex|Slice)Loop", "-test.benchmem", "-test.benchtime=200ms", "-test.count=1"], f"end-to-end-{variant}.txt", root / "internal/runtime")

for log in ("tests-simd.txt", "oracles-simd.txt", "vet-simd.txt"):
    (out / log).write_text("")
run(["go", "test", "-overlay", str(out / "overlays/simd.json"), "./...", "-count=1"], "tests-simd.txt")
run([str(out / "runtime-simd.test"), "-test.timeout=20m", "-test.count=1"], "oracles-simd.txt", root / "internal/runtime", {"VIBES_ESTIMATOR_VERIFY": "1", "VIBES_ENV_RECYCLE_VERIFY": "1"})
run(["go", "vet", "-overlay", str(out / "overlays/simd.json"), "./..."], "vet-simd.txt")
print(f"Measurements and checks saved in {out}")
