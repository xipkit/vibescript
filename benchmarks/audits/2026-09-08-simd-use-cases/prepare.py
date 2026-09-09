"""Generate overlays for string SIMD experiments without changing production code."""

import json
from pathlib import Path
import sys

here = Path(__file__).resolve().parent
root = here.parents[2]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
source = root / "internal/runtime/members_string.go"
original = source.read_text()


def declaration(text, name):
    start = text.index("func " + name + "(")
    end = text.index("\n}\n", start) + 3
    return text[start:end]


names = {
    "asciiUpcase": "Upcase",
    "asciiDowncase": "Downcase",
    "asciiSwapCase": "SwapCase",
    "asciiCapitalize": "Capitalize",
    "asciiCaseCompare": "CaseCompare",
    "asciiCaseEqual": "CaseEqual",
}
references = 'package runtime\n\nimport "strings"\n\n'
for name, suffix in names.items():
    references += declaration(original, name).replace("func " + name + "(", "func auditReference" + suffix + "(", 1) + "\n"
regexp_source = root / "internal/runtime/regexp_namespace.go"
regexp_original = regexp_source.read_text()
regexp_names = {"regexpQuotedSize": "RegexpQuotedSize", "writeRegexpQuoted": "WriteRegexpQuoted"}
for name, suffix in regexp_names.items():
    references += declaration(regexp_original, name).replace("func " + name + "(", "func auditReference" + suffix + "(", 1) + "\n"
for name, suffix in {
    "splitOnASCIIWhitespaceLimitProjection": "SplitProjection",
    "splitOnASCIIWhitespaceLimit": "SplitParts",
    "stringSplitWhitespaceResult": "SplitResult",
    "rubyLstrip": "Lstrip",
    "rubyRstrip": "Rstrip",
}.items():
    references += declaration(original, name).replace("func " + name + "(", "func auditReference" + suffix + "(", 1) + "\n"
(out / "reference_test.go").write_text(references)

common = {str(root / "internal/runtime/usecase_audit_reference_test.go"): str(out / "reference_test.go")}
for fixture in sorted(here.glob("*.go.txt")):
    common[str(root / ("internal/runtime/usecase_audit_" + fixture.name.removesuffix(".txt")))] = str(fixture)

space_loop = "for i < n && isRubyASCIISpace(text[i]) {\n\t\t\ti++\n\t\t}"
field_loop = "for i < n && !isRubyASCIISpace(text[i]) {\n\t\t\ti++\n\t\t}"

for variant in ("original", "go", "simd"):
    replacements = dict(common)
    if variant != "original":
        prefix = "auditSIMD" if variant == "simd" else "auditGo"
        modified = original
        for name, suffix in names.items():
            old = declaration(original, name)
            signature = old[:old.index("{\n")]
            args = "a, b" if suffix.startswith("Case") else "text"
            modified = modified.replace(old, signature + "{\n\treturn " + prefix + suffix + "(" + args + ")\n}\n", 1)
        # Projection and both materialization paths must see identical boundaries.
        for name in ("splitOnASCIIWhitespaceLimit", "splitOnASCIIWhitespaceLimitProjection", "stringSplitWhitespaceResult"):
            old = declaration(original, name)
            assert old.count(space_loop) == 1 and old.count(field_loop) == 1
            new = old.replace(space_loop, "i += " + prefix + "SpacePrefix(text[i:], false)")
            new = new.replace(field_loop, "i += " + prefix + "NonSpacePrefix(text[i:])")
            modified = modified.replace(old, new, 1)
        old = declaration(original, "rubyLstrip")
        modified = modified.replace(old, "func rubyLstrip(text string) string {\n\treturn text[" + prefix + "SpacePrefix(text, true):]\n}\n", 1)
        old = declaration(original, "rubyRstrip")
        modified = modified.replace(old, "func rubyRstrip(text string) string {\n\treturn text[:len(text)-" + prefix + "SpaceSuffix(text, true)]\n}\n", 1)
        path = out / f"members_string_{variant}.go"
        path.write_text(modified)
        replacements[str(source)] = str(path)
        modified = regexp_original
        for name, suffix in regexp_names.items():
            old = declaration(regexp_original, name)
            signature = old[:old.index("{\n")]
            call = prefix + suffix + "(text, limit)" if name == "regexpQuotedSize" else prefix + suffix + "(out, text)"
            if name == "regexpQuotedSize":
                call = "return " + call
            modified = modified.replace(old, signature + "{\n\t" + call + "\n}\n", 1)
        path = out / f"regexp_namespace_{variant}.go"
        path.write_text(modified)
        replacements[str(regexp_source)] = str(path)
    (out / f"{variant}.json").write_text(json.dumps({"Replace": replacements}, indent=2) + "\n")
print(out)
