"""Validate the integrated SIMD overlay after running measure.py."""

import os
from pathlib import Path
import subprocess
import sys

here = Path(__file__).resolve().parent
root = here.parents[2]
out = Path(sys.argv[1]).resolve()
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOEXPERIMENT="simd")
overlay = str(out / "overlays/simd.json")

checks = [
    ("tests-simd.txt", ["go", "test", "-p", "2", "-overlay", overlay, "./...", "-count=1"], {}),
    ("oracles-simd.txt", ["go", "test", "-p", "2", "-overlay", overlay, "./internal/runtime", "-count=1", "-timeout=20m"], {"VIBES_ESTIMATOR_VERIFY": "1", "VIBES_ENV_RECYCLE_VERIFY": "1"}),
    ("vet-simd.txt", ["go", "vet", "-p", "2", "-overlay", overlay, "./..."], {}),
]
for name, args, extra in checks:
    with (out / name).open("w") as stream:
        subprocess.run(args, cwd=root, env=dict(env, **extra), stdout=stream, stderr=subprocess.STDOUT, check=True)
    print(f"Passed {name}", flush=True)
