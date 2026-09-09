import json
import os
from pathlib import Path
import subprocess
import sys


BASE = "7d1563d9cebd38b64a3bd5fb9e35000bd4a20da3"
FUNCTIONS = (
    "splitOnASCIIWhitespaceLimit",
    "splitOnASCIIWhitespaceLimitProjection",
    "stringSplitWhitespaceResult",
    "rubyLstrip",
    "rubyRstrip",
)


def function(source, name):
    start = source.index("func " + name + "(")
    end = source.index("\n}\n", start) + 3
    return source[start:end]


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: measure.py OUTPUT_DIRECTORY [TRIALS]")
    root = Path(__file__).resolve().parents[3]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    trials = int(sys.argv[2]) if len(sys.argv) > 2 else 10
    if trials < 0:
        raise SystemExit("TRIALS must be non-negative")
    env = {**os.environ, "GOTOOLCHAIN": "go1.27.1", "GOEXPERIMENT": "simd"}
    member_path = root / "internal/runtime/members_string.go"
    current = member_path.read_text()
    baseline = subprocess.check_output(
        ["git", "show", BASE + ":internal/runtime/members_string.go"], cwd=root, text=True
    )
    original = current
    for name in FUNCTIONS:
        original = original.replace(function(current, name), function(baseline, name), 1)
    (output / "members_string_original.go").write_text(original)
    control = (root / "internal/runtime/whitespace_scan_simd.go").read_text()
    assert control.count("const whitespaceSIMD = true") == 1
    (output / "whitespace_control.go").write_text(
        control.replace("const whitespaceSIMD = true", "const whitespaceSIMD = false")
    )
    accounting = Path(__file__).resolve().with_name("accounting_test.go.txt")
    variants = ["original", "go", "simd"]
    snapshots = None
    for variant in variants:
        mapping = {str(root / "internal/runtime/whitespace_audit_test.go"): str(accounting)}
        if variant == "original":
            mapping[str(member_path)] = str(output / "members_string_original.go")
        if variant == "go":
            mapping[str(root / "internal/runtime/whitespace_scan_simd.go")] = str(output / "whitespace_control.go")
        overlay = output / (variant + ".json")
        overlay.write_text(json.dumps({"Replace": mapping}, indent=2) + "\n")
        binary = output / ("runtime-" + variant + ".test")
        subprocess.run(
            ["go", "test", "-p", "2", "-overlay", str(overlay), "-c", "-o", str(binary), "./internal/runtime"],
            cwd=root, env=env, check=True,
        )
        result = subprocess.run(
            [str(binary), "-test.run=^TestAuditAccountingSnapshot$", "-test.v"],
            cwd=root, env=env, check=True, capture_output=True, text=True,
        )
        (output / ("account-" + variant + ".txt")).write_text(result.stdout)
        rows = sorted(line[line.index("AUDIT_ACCOUNT "):] for line in result.stdout.splitlines() if "AUDIT_ACCOUNT " in line)
        if len(rows) != 84:
            raise SystemExit(f"{variant}: expected 84 accounting snapshots, got {len(rows)}")
        if snapshots is None:
            snapshots = rows
        elif snapshots != rows:
            raise SystemExit(f"{variant}: accounting snapshots differ from original")
        (output / ("bench-" + variant + ".txt")).write_text("")
        print(f"{variant}: 84 accounting snapshots match", flush=True)
    for trial in range(trials):
        order = variants[trial % 3:] + variants[:trial % 3]
        for variant in order:
            with (output / ("bench-" + variant + ".txt")).open("a") as log:
                subprocess.run(
                    [str(output / ("runtime-" + variant + ".test")), "-test.run=^$", "-test.bench=^BenchmarkWhitespace", "-test.benchtime=100ms", "-test.count=1"],
                    cwd=root, env=env, check=True, stdout=log,
                )
        print(f"completed trial {trial + 1}/{trials}", flush=True)


if __name__ == "__main__":
    main()
