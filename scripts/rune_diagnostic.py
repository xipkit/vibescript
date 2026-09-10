"""Native, ordinary-executable measurements for issue 1309."""

from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess


out = Path(os.environ["RUNNER_TEMP"]) / "rune-results"
out.mkdir()
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOAMD64="v1", GOMAXPROCS="1")
cpu = min(os.sched_getaffinity(0))
manifest = {"revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
            "cpu": cpu, "samples": [], "binaries": []}
with (out / "environment.txt").open("w") as log:
    for cmd in [["uname", "-a"], ["lscpu"], ["go", "version"], ["go", "env", "GOARCH", "GOAMD64"]]:
        subprocess.run(cmd, env=env, stdout=log, check=True)

def save():
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

for experiment in ["nosimd", "simd"]:
    options = dict(env, GOEXPERIMENT=experiment)
    for seed in range(17):
        name = f"{experiment}-{seed}"
        binary = out / name
        cmd = ["go", "build", "-o", str(binary)]
        if seed:
            cmd.append(f"-ldflags=-randlayout={seed}")
        cmd.append("./scripts/rune_probe")
        subprocess.run(cmd, env=options, check=True)
        manifest["binaries"].append({"name": name, "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()})
        with (out / f"{name}.asm").open("w") as log:
            subprocess.run(["go", "tool", "objdump", "-s", "(^main.count|^runtime.decoderune$|^unicode/utf8.ValidString$|stringMemberQuery.func2$|validUTF8RuneCount$|validUTF8ByteIndex$)", str(binary)], env=options, stdout=log, check=True)
        patterns = [
            ("kernels", "^Benchmark(Range|Validated|TwoByte|Widths)$/(Unicode|CJK|Emoji|InvalidSuffix)$", 16),
            ("calls", "^BenchmarkCalls$/(length|index.*|rindex.*)$/(ASCII|Unicode)$", 6),
        ]
        for trial in range(2):
            for group, pattern, cases in patterns if trial == 0 else reversed(patterns):
                cmd = ["taskset", "-c", str(cpu), str(binary), "-test.run=^$", "-test.bench=" + pattern,
                       "-test.benchmem", "-test.cpu=1", "-test.benchtime=150ms", "-test.count=1"]
                sample = subprocess.check_output(cmd, env=options, text=True)
                names = [line.split()[0] for line in sample.splitlines() if line.startswith("Benchmark") and "ns/op" in line]
                assert len(names) == len(set(names)) == cases, names
                with (out / f"{name}-{group}.txt").open("a") as log:
                    log.write(sample)
                manifest["samples"].append({"name": name, "trial": trial, "group": group, "cases": cases, "names": names})
                save()
        if seed == 0:
            cmd = ["taskset", "-c", str(cpu), str(binary), "-test.run=^$", "-test.bench=^BenchmarkCalls$/^length$/^Unicode$", "-test.benchtime=3s"]
            subprocess.run(cmd, env=dict(options, RUNE_CPU_PROFILE=str(out / f"{name}.prof")), check=True)
            for flags, suffix in [(["-top"], "top"), (["-disasm", "stringMemberQuery.func2"], "disasm")]:
                with (out / f"{name}-{suffix}.txt").open("w") as log:
                    subprocess.run(["go", "tool", "pprof", *flags, str(binary), str(out / f"{name}.prof")], env=options, stdout=log, check=True)
            with (out / f"{name}-perf.txt").open("w") as log:
                if shutil.which("perf"):
                    attempt = subprocess.run(["perf", "stat", "-e", "cycles,instructions,branches,branch-misses,L1-icache-load-misses", "--", *cmd], env=options, stdout=log, stderr=subprocess.STDOUT)
                    log.write(f"\nperf exit status: {attempt.returncode}\n")
                else:
                    log.write("perf is unavailable on this runner\n")
        binary.unlink()
        print("Complete", name, flush=True)
save()
