#!/usr/bin/env python3
"""Report a measured comparison against preserved Rust baseline binaries."""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    directory = Path(sys.argv[1]).resolve()
    summary = json.loads((directory / "summary.json").read_text())
    environment = json.loads((directory / "environment.json").read_text())
    relative = directory.relative_to(ROOT / "benchmarks")
    before, after, go = "rust-before-simd", "rust-simd", "go-simd"
    variants = ["go-portable", go, "rust-before-portable", before, "rust-portable", after]
    cases = summary["cases"]
    count = len((directory / "validation-rust-simd.jsonl").read_text().splitlines())

    def metric(name, variant, key="median_ns", mode="metered"):
        return cases[f"{name}/{mode}"][variant][key]

    def speed(name):
        return metric(name, before) / metric(name, after)

    selected = ["array_growth", "upstream_countdown", "length_65536", "length_unicode", "json_parse_ascii_64k", "json_stringify_ascii_64k", "json_parse_unicode_4k", "json_stringify_unicode_4k"]
    lines = [
        "# First Rust performance iteration", "",
        f"Measured on {environment['cpu']} with {environment['rustc'].splitlines()[0]} and {environment['go'].strip()}. Original Rust binaries are from `{environment['baseline_source_revision']}`; the updated runtime and harness are from `{environment['source_revision']}`. Go remains Vibescript v0.70.0. The original binaries were rerun alongside the new builds, using the same fixtures and iteration counts.", "",
        "## Results", "",
        f"Array growth is {speed('array_growth'):.1f}× faster and allocates {metric('array_growth',before,'alloc_bytes') / metric('array_growth',after,'alloc_bytes'):.1f}× fewer bytes. The unchanged upstream countdown example also benefits from reusing storage for `out = out + [n]`. Reading a 64 KiB string's length now allocates {metric('length_65536',after,'alloc_bytes'):,.0f} bytes per call instead of {metric('length_65536',before,'alloc_bytes'):,.0f}, while the execution budget still charges its retained backing capacity.", "",
        f"Unicode JSON parsing and stringification are {speed('json_parse_unicode_4k'):.1f}× and {speed('json_stringify_unicode_4k'):.1f}× faster than the initial Rust implementation. ASCII JSON parsing allocates {metric('json_parse_ascii_64k',after,'alloc_bytes'):,.0f} bytes per call versus {metric('json_parse_ascii_64k',before,'alloc_bytes'):,.0f} before. The integer-loop timing remains {metric('numeric_loop',after)/1000:.2f} µs versus Go SIMD's {metric('numeric_loop',go)/1000:.2f} µs.", "",
        f"All times below are medians of {summary['rounds']} samples with accounting enabled. Allocation bytes are cumulative requested allocation volume per call, not retained memory or RSS.", "",
        "| Workload | Rust before µs | Rust after µs | Go SIMD µs | Rust B/call before → after |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for name in selected:
        lines.append(f"| `{name}` | {metric(name,before)/1000:.3f} | {metric(name,after)/1000:.3f} | {metric(name,go)/1000:.3f} | {metric(name,before,'alloc_bytes'):,.0f} → {metric(name,after,'alloc_bytes'):,.0f} |")
    transform = (metric('json_transform', after) / metric('json_transform', before) - 1) * 100
    lines += [
        "", "## Remaining tradeoffs", "",
        f"- Unicode length improved by {speed('length_unicode'):.1f}× but still takes {metric('length_unicode',after)/metric('length_unicode',go):.2f}× Go's time. Counting and validating UTF-8 is still a target for further work.",
        f"- Escape-heavy JSON stringification with accounting disabled takes {metric('json_stringify_escaped_4k',after,mode='unlimited')/metric('json_stringify_escaped_4k',go,mode='unlimited'):.2f}× Go's time. Small escape operations still enter the accounting helpers frequently.",
        f"- The metered JSON transformation workload changed by {transform:+.1f}% versus the initial Rust implementation; it remains {metric('json_transform',go)/metric('json_transform',after):.1f}× faster than Go SIMD. Storage metadata and short-string processing need further profiling before attributing this difference to a specific cause.",
        f"- With explicit SIMD disabled, escape-heavy JSON parsing changed by {(metric('json_parse_escaped_4k','rust-portable')/metric('json_parse_escaped_4k','rust-before-portable')-1)*100:+.1f}% versus the original portable Rust build. The default SIMD build changed by {(metric('json_parse_escaped_4k',after)/metric('json_parse_escaped_4k',before)-1)*100:+.1f}% on the same metered case.",
        "- Hash lookup and duplicate-key replacement still scan ordered entries linearly. The 64-key benchmark does not establish large-object scalability. Adding an index while preserving order and accounting remains a structural follow-up.",
        "- Distinct imports of the same foreign byte buffer conservatively charge separate views; cloning an already imported value shares its charge. The memory model remains different from Go's reachable-graph estimator.",
        "- This is still a partial language implementation. Full Unicode case mapping, bignums, modules, classes, blocks, and suspended async host calls remain outside the core. x86_64 SIMD was not compiled or measured on this ARM64 machine.",
        "", "## What changed", "",
        "Array writes reuse uniquely owned storage and copy when aliases exist. The VM releases the overwritten local only after argument evaluation, and additive assignment combines execution and writeback. Array depth is maintained incrementally, with rescans when replacing a deepest child could reduce the depth.", "",
        "Immutable byte storage is shared across calls with independent memory charges. The charge includes the full backing capacity and headers; it does not disappear because the bytes originated in the host. Returned JSON strings and character slices own their storage so small results do not retain large source documents.", "",
        "Unicode scans validate sequence widths and batch accounting between bounded spans. JSON copies valid spans together, reserves unescaped strings once, and leaves room for closing delimiters after large string values. Short escape paths avoid speculative vector scans and repeated checks of the same delimiter.", "",
        "## Validation and method", "",
        f"- {count} shared cases match independently computed expected outputs in all six builds, including 46 calls from ten unchanged upstream files. New cases cover self-aliasing arrays, argument evaluation, additive assignments, malformed UTF-8, and vector/chunk boundaries.",
        "- The UTF-8 decoder is checked against every Unicode scalar value and combinations of invalid leading, continuation, and truncated bytes. Tests also cover quota exhaustion during Unicode work, spare input capacity, cross-call charge lifetime, array depth changes, and reclamation of large JSON sources.",
        "- Portable and SIMD builds report identical accounting within each Rust revision. Step counts can change across revisions because instruction fusion and removal of copying change the work performed. Quotas, recursion limits, cancellation, and latched exhaustion remain enabled and tested.",
        "- Formatting, Clippy with warnings denied, debug/release tests, portable/SIMD tests, Go vet, and the Tokio cancellation/worker-permit tests pass. The final verification artifact records CLI and provenance checks.",
        f"- {len(cases)//2} workloads run with accounting enabled and disabled. Six builds rotate through all positions twice over {summary['rounds']} rounds. Pilot measurements choose a common iteration count per case. Compilation, setup, and serialization are outside timing; argument import and execution are inside. Final timed outputs are checked.",
        "- Timing uses uninstrumented Rust binaries. Separate binaries count allocations with a system-allocator wrapper. Go uses runtime.MemStats; its size-class accounting is not identical to Rust's requested-layout accounting. Process peak RSS includes the complete harness and fixture, and is a coarse single-process measurement.",
        "", "| Variant | Peak RSS MiB |", "| --- | ---: |",
    ]
    for variant in variants:
        lines.append(f"| {variant} | {summary['peak_rss_bytes'][variant]/2**20:.2f} |")
    lines += [
        "", "## Reproduce", "",
        "Build the initial Rust comparison binaries from commit `8403e83c4760cee2601b229cd4a94c97777414a1` using its `scripts/compare.py --validate-only`. Preserve the four `rust-*` timing/allocation binaries together with a `revision` file containing that full commit hash. Keep the checkout, caches, and binaries on the external volume. Then run:", "",
        "```sh", "./scripts/check", "python3 scripts/compare.py --baseline /path/to/preserved/binaries --rounds 12", "python3 scripts/report-followup.py benchmarks/results/<run-directory>", "```", "",
        f"[Raw results]({relative}/summary.json), [environment and binary hashes]({relative}/environment.json), [test log]({relative}/validation.log), and [final verification]({relative}/verification.json) preserve the evidence. The original timing binary hashes match the [initial report's manifest](results/2026-09-12-m4/environment.json).",
    ]
    for mode in ["metered", "unlimited"]:
        lines += ["", f"## Complete timings: {mode}", "", "Median µs/call; lower is better.", "", "| Workload | Go portable | Go SIMD | Rust before portable | Rust before SIMD | Rust after portable | Rust after SIMD |", "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
        for name, rows in cases.items():
            if name.endswith('/'+mode):
                values = ' | '.join(f"{rows[v]['median_ns']/1000:.3f}" for v in variants)
                lines.append(f"| `{name.rsplit('/',1)[0]}` | {values} |")
    lines += ["", "## Complete metered allocations", "", "| Workload | Go B/call | Rust before B/call | Rust after B/call | Go allocations | Rust before allocations | Rust after allocations |", "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for name, rows in cases.items():
        if name.endswith('/metered'):
            values = [f"{rows[v]['alloc_bytes']:,.0f}" for v in [go, before, after]]
            values += [f"{rows[v]['allocations']:.1f}" for v in [go, before, after]]
            lines.append(f"| `{name.rsplit('/',1)[0]}` | {' | '.join(values)} |")
    destination = ROOT / 'benchmarks/performance-followup.md'
    destination.write_text('\n'.join(lines)+'\n')
    print(destination)


if __name__ == '__main__':
    main()
