#!/usr/bin/env python3
"""Render the comparison report from measured data, preserving raw samples."""
import json
import sys
from pathlib import Path

ROOT=Path(__file__).resolve().parent.parent
VARIANTS=["go-portable","go-simd","rust-portable","rust-simd"]


def main():
    directory=Path(sys.argv[1]).resolve()
    result=json.loads((directory/"summary.json").read_text())
    env=json.loads((directory/"environment.json").read_text())
    relative=directory.relative_to(ROOT/"benchmarks")
    cases=result["cases"]
    rust_version=env["rustc"].splitlines()[0]
    go_version=env["go"].strip()

    def metric(name,variant,key="median_ns",mode="metered"):
        return cases[f"{name}/{mode}"][variant][key]

    def speed(name,a="go-simd",b="rust-simd"):
        return metric(name,a)/metric(name,b)

    def us(name,variant):
        return f"{metric(name,variant)/1000:.2f}"

    lines=["# Rust core performance comparison", "",
        f"Measured on {env['cpu']} ({env['platform']}) with `{rust_version}` and `{go_version}`. Go uses Vibescript {env['go_module']['Version']}; the Rust implementation and harness are pinned at `{env['source_revision']}`. [Raw results and environment]("+str(relative)+"/environment.json).", "",
        "## Findings", "",
        f"The Rust bytecode core is faster on the integer loop ({speed('numeric_loop'):.1f}×) and the unchanged upstream Fibonacci example ({speed('upstream_fibonacci'):.1f}×) with accounting enabled. This compares a small bytecode VM with the full Go interpreter, so the result includes architecture, feature coverage, and accounting differences. It does not isolate the effect of the implementation language.", "",
        f"For a 64 KiB ASCII payload, Rust with SIMD parses JSON in {us('json_parse_ascii_64k','rust-simd')} µs versus Go with SIMD at {us('json_parse_ascii_64k','go-simd')} µs, and stringifies it in {us('json_stringify_ascii_64k','rust-simd')} µs versus {us('json_stringify_ascii_64k','go-simd')} µs. Within Rust, enabling the explicit SIMD scanners improves these calls by {speed('json_parse_ascii_64k','rust-portable'):.1f}× and {speed('json_stringify_ascii_64k','rust-portable'):.1f}×. Go's SIMD experiment improves the same calls by {speed('json_parse_ascii_64k','go-portable','go-simd'):.1f}× and {speed('json_stringify_ascii_64k','go-portable','go-simd'):.1f}×.", "",
        f"Unicode is a weakness of this first Rust decoder: Unicode length is {1/speed('length_unicode'):.1f}× slower than Go with SIMD, and Unicode JSON parsing/stringification are {1/speed('json_parse_unicode_4k'):.1f}×/{1/speed('json_stringify_unicode_4k'):.1f}× slower. Explicit SIMD case conversion also takes {metric('upcase_65536','rust-simd')/metric('upcase_65536','rust-portable'):.2f}× the time of the portable Rust loop on the 64 KiB upcase case. There is no general benefit from manually vectorizing every operation; the portable build still permits LLVM auto-vectorization.", "",
        f"Memory results are mixed. The Rust integer loop allocates {metric('numeric_loop','rust-simd','alloc_bytes'):,.0f} bytes per call versus Go's {metric('numeric_loop','go-simd','alloc_bytes'):,.0f}. However, repeated array growth allocates {metric('array_growth','rust-simd','alloc_bytes'):,.0f} versus {metric('array_growth','go-simd','alloc_bytes'):,.0f} bytes ({metric('array_growth','rust-simd','alloc_bytes')/metric('array_growth','go-simd','alloc_bytes'):.1f}× more), and ASCII JSON parsing allocates {metric('json_parse_ascii_64k','rust-simd','alloc_bytes')/metric('json_parse_ascii_64k','go-simd','alloc_bytes'):.1f}× more. Fast execution alone would conceal these costs.", "",
        "## Selected calls with accounting enabled", "",
        f"Median of {result['rounds']} samples. Lower time and allocation volume are better. Bytes are cumulative allocation volume per call, not retained heap or RSS.", "",
        "| Workload | Go SIMD µs | Rust SIMD µs | Go B/call | Rust B/call |",
        "| --- | ---: | ---: | ---: | ---: |"]
    for name in ["numeric_loop","array_growth","length_65536","length_unicode","upcase_65536","json_parse_ascii_64k","json_stringify_ascii_64k","json_parse_unicode_4k","json_transform","upstream_fibonacci","upstream_countdown"]:
        lines.append(f"| `{name}` | {us(name,'go-simd')} | {us(name,'rust-simd')} | {metric(name,'go-simd','alloc_bytes'):,.0f} | {metric(name,'rust-simd','alloc_bytes'):,.0f} |")
    lines += ["", "## Process memory", "",
        "One separate process per variant runs the entire 48-case measurement fixture for 100 calls per case. These are macOS maximum resident set sizes from `/usr/bin/time -l`, including runtime, fixture decoding, compiled code, and harness overhead. They are coarse process measurements, not per-call memory limits or a language-wide memory comparison.", "",
        "| Variant | Peak RSS MiB |", "| --- | ---: |"]
    for variant in VARIANTS:
        lines.append(f"| {variant} | {result['peak_rss_bytes'][variant]/2**20:.2f} |")
    lines += ["", "## Method and validation", "",
        "- Four builds on the same native ARM64 machine: Go 1.27.1 without the SIMD experiment, Go 1.27.1 with `GOEXPERIMENT=simd`, Rust portable scanners, and Rust explicit NEON scanners. Rust uses release optimization, thin LTO, and one codegen unit. No `target-cpu=native` override was added. x86_64 SSE2 code is present but was not compiled or measured on this machine.",
        "- 145 shared invocations match independently computed expected results on all four builds. These include 46 invocations of ten complete, unchanged upstream `.vibe` files. SHA-256 hashes pin those files. Three original examples also appear in the benchmark suite.",
        "- 24 workloads run with accounting enabled and disabled, for 48 measured cases. Enabled limits are five million steps, 64 MiB, and 256 frames; disabled runs retain a recursion limit and cooperative cancellation. Rust and Go charge different units and use different memory models, so equal numeric limits are not equivalent sandbox policies.",
        "- Compilation, fixture decoding, argument construction, output encoding, and validation are outside the timer. Per-call argument import, execution, result construction, and normal per-call runtime work are included. Both harnesses retain the last result, and verify it against the initial result after timing. Results are also checked against expected outputs before measurement.",
        "- Pilot runs select one iteration count shared by all variants for each case. Eight rounds rotate execution order so every build occupies every position twice. Go uses `GOMAXPROCS=1`; Rust executes one synchronous call at a time. The raw samples preserve min/max and run order; small percentage differences should be treated cautiously.",
        "- Rust allocation measurement uses separate instrumented binaries wrapping the system allocator; its timing numbers are not used in the timing tables. Rust counts requested layout bytes and treats reallocation as another allocation. Go uses `runtime.MemStats.TotalAlloc` and `Mallocs`, including its size-class behavior. These are useful allocation-volume comparisons but not identical physical-memory accounting.",
        "- Rust SIMD and portable builds report identical steps, peak tracked capacity, and retained tracked capacity on all shared invocations. Separate tests cover step/memory/recursion/deadline exhaustion, cancellation before argument import and during execution, ignored host quota errors, temporary/frame reclamation, JSON source retention, and Tokio worker-permit lifetime after cancellation.",
        "- Debug/release tests, portable/SIMD tests, Clippy with warnings denied, formatting, Go vet, and CLI examples pass. This remains a partial interpreter: classes, blocks, typing, bignums, modules, most standard-library methods, and native async host callbacks are outside the implemented core.", "",
        "## Next performance work", "",
        "1. Share immutable host byte buffers while charging their retained storage. The current argument importer copies strings; even a length call allocates a full input copy, while Go shares string storage.",
        "2. Reuse unshared array storage for updates with copy-on-write behavior when aliases exist. The current immutable implementation copies arrays on every push or indexed write, which explains the array-growth allocation volume and quadratic copying.",
        "3. Improve valid-Unicode scans and JSON span handling while preserving invalid-byte behavior and chunk checkpoints. The current decoder repeatedly classifies and validates individual runes.",
        "4. Inspect code generation before adding more explicit SIMD case-conversion paths. Keep the portable control: the existing Rust loop is already competitive with the manually vectorized implementation on this machine.", "",
        "These are measured follow-up targets, not claims that a full Rust port will retain the same advantages. [Rust SIMD documentation](https://doc.rust-lang.org/std/arch/index.html) explains the distinction between explicit intrinsics and compiler auto-vectorization; [Go build documentation](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/building.md) describes the optional experiment.", "",
        "## Reproduce", "", "```sh", "./scripts/check", "python3 scripts/compare.py --rounds 8", "python3 scripts/report.py benchmarks/results/<run-directory>", "```", "",
        f"The recorded run is in [`{relative}`]({relative}/summary.json). Build hashes and module provenance are in `environment.json`; `validation-*.jsonl`, `round-*.jsonl`, `allocations-*.jsonl`, and `rss-*.txt` preserve the underlying evidence. The [final audit]({relative}/verification.json) records source and binary checks, upstream file verification, and CLI results; [the validation log]({relative}/validation.log) preserves the full local test gate."]
    for mode in ["metered","unlimited"]:
        lines += ["",f"## Complete timings: {mode}","","Median µs/call; lower is better.","","| Workload | Go portable | Go SIMD | Rust portable | Rust SIMD |","| --- | ---: | ---: | ---: | ---: |"]
        for name,variants in cases.items():
            if name.endswith('/'+mode):
                values=" | ".join(f"{variants[v]['median_ns']/1000:.3f}" for v in VARIANTS)
                lines.append(f"| `{name.rsplit('/',1)[0]}` | {values} |")
    lines += ["","## Complete allocations with accounting enabled","","| Workload | Go B/call | Rust B/call | Go allocations/call | Rust allocations/call |","| --- | ---: | ---: | ---: | ---: |"]
    for name,variants in cases.items():
        if name.endswith('/metered'):
            go=variants['go-simd'];rust=variants['rust-simd']
            lines.append(f"| `{name.rsplit('/',1)[0]}` | {go['alloc_bytes']:,.0f} | {rust['alloc_bytes']:,.0f} | {go['allocations']:.1f} | {rust['allocations']:.1f} |")
    destination=ROOT/"benchmarks/README.md"
    destination.write_text('\n'.join(lines)+'\n')
    print(destination)


if __name__=="__main__": main()
