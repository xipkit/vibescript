"""Create test overlays without modifying production sources (Go 1.27.1 ARM64)."""

import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[3]
source = root / "internal/runtime/members_string.go"
probe = Path(__file__).with_name("probe_test.go.txt")
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
original = source.read_text()
start = original.index("func stringIsASCII(text string) bool {")
end = original.index("\n}\n", start) + 3

for variant in ("scalar", "word", "simd"):
    replacements = {str(root / "internal/runtime/simd_audit_test.go"): str(probe)}
    if variant != "scalar":
        body = probe.read_text()
        name = "auditWordASCII" if variant == "word" else "auditSIMDASCII"
        begin = body.index(f"func {name}(text string) bool {{")
        finish = body.index("\n}\n", begin) + 3
        helper = body[begin:finish].replace(name, "stringIsASCII")
        helper = helper.replace("return auditScalarASCII(text)", """for i := range len(text) {
        if text[i] >= 128 { return false }
    }
    return true""")
        modified = original[:start] + helper + original[end:]
        dependency = "encoding/binary" if variant == "word" else "simd/archsimd"
        modified = modified.replace("import (", f'import (\n\t"{dependency}"', 1)
        path = out / f"members_string_{variant}.go"
        path.write_text(modified)
        replacements[str(source)] = str(path)
    (out / f"{variant}.json").write_text(json.dumps({"Replace": replacements}, indent=2) + "\n")
    if variant == "word":
        portable = {k: v for k, v in replacements.items() if not k.endswith("_test.go")}
        (out / "word-portable.json").write_text(json.dumps({"Replace": portable}, indent=2) + "\n")
print(out)
