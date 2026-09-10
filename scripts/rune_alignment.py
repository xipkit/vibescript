"""Move one function inside fixed padding, keeping all other text PCs fixed."""

from pathlib import Path
import hashlib
import json
import os
import subprocess
import sys


out = Path(os.environ["RUNNER_TEMP"]) / "rune-alignment"
out.mkdir()
env = dict(os.environ, GOTOOLCHAIN="go1.27.1", GOAMD64="v1", GOMAXPROCS="1", CGO_ENABLED="0")
cpu = min(os.sched_getaffinity(0))
goroot = Path(subprocess.check_output(["go", "env", "GOROOT"], env=env, text=True).strip())
overlay = {}
for filename in ["main.go", "data.go"]:
    source = goroot / "src/cmd/link/internal/ld" / filename
    original = source.read_text()
    if filename == "main.go":
        needle = '\tFlagFuncAlign     = flag.Int("funcalign", 0, "set function align to `N` bytes")'
        replacement = needle + '\n\tflagRuneTarget = flag.String("runetarget", "", "diagnostic symbol to move")\n\tflagRunePad = flag.Int("runepad", 0, "diagnostic byte offset within fixed 64-byte padding")'
    else:
        needle = '\tldr.SetSymValue(s, 0)\n\tfor sub := s; sub != 0; sub = ldr.SubSym(sub) {'
        replacement = '''\tmoveRune := ldr.SymName(s) == *flagRuneTarget
\tif moveRune {
\t\tif *flagRunePad < 0 || *flagRunePad >= 64 { Exitf("invalid rune padding") }
\t\tva += uint64(*flagRunePad)
\t}
''' + needle
        assert original.count('\tva += funcsize\n') == 1
        original = original.replace('\tva += funcsize\n', '\tva += funcsize\n\tif moveRune { va += 64 - uint64(*flagRunePad) }\n')
    assert original.count(needle) == 1
    modified = out / filename
    modified.write_text(original.replace(needle, replacement))
    overlay[str(source)] = str(modified)
(out / "overlay.json").write_text(json.dumps({"Replace": overlay}, indent=2) + "\n")
linker = out / "link"
subprocess.run(["go", "build", "-overlay", str(out / "overlay.json"), "-o", str(linker), "cmd/link"], env=env, check=True)
wrapper = out / "toolexec.py"
wrapper.write_text('#!/usr/bin/env python3\nimport os, sys\nfrom pathlib import Path\ntool = ' + repr(str(linker)) + ' if Path(sys.argv[1]).name == "link" else sys.argv[1]\nos.execv(tool, [tool, *sys.argv[2:]])\n')
wrapper.chmod(0o755)
targets = {
    "length": "github.com/mgomes/vibescript/internal/runtime.stringMemberQuery.func2",
    "decoder": "runtime.decoderune",
    "validation": "unicode/utf8.ValidString",
}
manifest = {"revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(), "cpu": cpu, "binaries": [], "samples": []}
with (out / "environment.txt").open("w") as log:
    for cmd in [["uname", "-a"], ["lscpu"], ["go", "version"]]:
        subprocess.run(cmd, env=env, stdout=log, check=True)

def save():
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

for experiment in ["nosimd", "simd"]:
    options = dict(env, GOEXPERIMENT=experiment)
    for target, symbol in targets.items():
        binaries = {}
        symbol_reference = None
        for pad in [0, 16, 32, 48]:
            name = f"{experiment}-{target}-{pad}"
            binary = out / name
            subprocess.run(["go", "build", "-toolexec=" + str(wrapper), f"-ldflags=-runetarget={symbol} -runepad=0x{pad:02x}", "-o", str(binary), "./scripts/rune_probe"], env=options, check=True)
            nm = subprocess.check_output(["go", "tool", "nm", "-size", str(binary)], env=options, text=True)
            (out / f"{name}.nm").write_text(nm)
            text_symbols = {row[3]: int(row[0], 16) for line in nm.splitlines() if len(row := line.split()) == 4 and row[2] in ["t", "T"]}
            assert symbol in text_symbols
            if symbol_reference is None:
                symbol_reference = text_symbols
            else:
                changes = {sym: pc - symbol_reference[sym] for sym, pc in text_symbols.items() if pc != symbol_reference[sym]}
                assert changes == {symbol: pad}, changes
            with (out / f"{name}.asm").open("w") as log:
                subprocess.run(["go", "tool", "objdump", "-s", "^" + symbol.replace(".", "\\.") + "$", str(binary)], env=options, stdout=log, check=True)
            manifest["binaries"].append({"name": name, "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "target_pc": text_symbols[symbol], "text_symbols": len(text_symbols), "only_target_moved": True})
            binaries[pad] = binary
            save()
        for trial in range(6):
            pads = [0, 16, 32, 48]
            pads = pads[trial % 4:] + pads[:trial % 4]
            for pad in pads:
                modes = [(experiment, options)]
                if experiment == "simd":
                    modes.append(("simd-avx2-disabled", dict(options, GODEBUG="cpu.avx2=off")))
                for mode, run_env in modes:
                    cmd = ["taskset", "-c", str(cpu), str(binaries[pad]), "-test.run=^$", "-test.bench=^BenchmarkCalls$/^(length|index.*|rindex.*)$/^Unicode$", "-test.benchtime=200ms", "-test.cpu=1", "-test.benchmem"]
                    sample = subprocess.check_output(cmd, env=run_env, text=True)
                    names = [line.split()[0] for line in sample.splitlines() if line.startswith("Benchmark") and "ns/op" in line]
                    assert len(names) == len(set(names)) == 3, names
                    name = f"{mode}-{target}-{pad}"
                    with (out / f"{name}.txt").open("a") as log:
                        log.write(sample)
                    manifest["samples"].append({"name": name, "trial": trial, "cases": names})
                    save()
        for pad, binary in binaries.items():
            if experiment == "simd" and target == "length":
                with (out / f"{experiment}-{target}-{pad}-perf.txt").open("w") as log:
                    cmd = ["sudo", "-n", "perf", "stat", "-e", "cycles,instructions,branches,branch-misses,L1-icache-load-misses", "--", "taskset", "-c", str(cpu), str(binary), "-test.run=^$", "-test.bench=^BenchmarkCalls$/^length$/^Unicode$", "-test.benchtime=2000x", "-test.cpu=1"]
                    attempt = subprocess.run(cmd, env=options, stdout=log, stderr=subprocess.STDOUT)
                    log.write(f"\nperf exit status: {attempt.returncode}\n")
            binary.unlink()
        print("Complete", experiment, target, flush=True)
linker.unlink()
save()
