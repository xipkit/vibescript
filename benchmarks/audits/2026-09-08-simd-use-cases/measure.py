"""Run serial, rotating-order public-call benchmarks on Go 1.27.1 ARM64."""

import os
from pathlib import Path
import subprocess
import sys

here = Path(__file__).resolve().parent
root = here.parents[2]
out = Path(sys.argv[1]).resolve()
rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 10
duration = sys.argv[3] if len(sys.argv) > 3 else "100ms"
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOEXPERIMENT="simd")
subprocess.run([sys.executable, str(here / "prepare.py"), str(out / "overlays")], check=True)


def run(args, log=None, cwd=root, extra_env=None):
    settings = dict(env, **(extra_env or {}))
    if log is None:
        subprocess.run(args, cwd=cwd, env=settings, check=True)
    else:
        with (out / log).open("a") as stream:
            subprocess.run(args, cwd=cwd, env=settings, stdout=stream, stderr=subprocess.STDOUT, check=True)


variants = ["original", "go", "simd"]
snapshots = []
for variant in variants:
    for log in (f"bench-{variant}.txt", f"parity-{variant}.txt"):
        (out / log).write_text("")
    run(["go", "test", "-c", "-overlay", str(out / f"overlays/{variant}.json"), "-o", str(out / f"runtime-{variant}.test"), "./internal/runtime"])
    run([str(out / f"runtime-{variant}.test"), "-test.run=^TestAudit", "-test.count=1"], f"parity-{variant}.txt", root / "internal/runtime")
    snapshots.append([line for line in (out / f"parity-{variant}.txt").read_text().splitlines() if line.startswith("AUDIT_ACCOUNT ")])
    if not snapshots[-1]:
        raise RuntimeError("Accounting snapshots missing")
if snapshots[1:] != [snapshots[0], snapshots[0]]:
    raise RuntimeError("Accounting snapshots differ across variants; inspect parity logs")
print(f"Accounting snapshots match across all variants: {len(snapshots[0])} cases", flush=True)

for trial in range(rounds):
    for variant in variants[trial % 3:] + variants[:trial % 3]:
        run([str(out / f"runtime-{variant}.test"), "-test.run=^$", "-test.bench=^BenchmarkAuditUseCases$", "-test.benchmem", f"-test.benchtime={duration}", "-test.count=1"], f"bench-{variant}.txt", root / "internal/runtime")
    print(f"Completed benchmark round {trial + 1}/{rounds}", flush=True)

print(f"Measurements saved in {out}")
