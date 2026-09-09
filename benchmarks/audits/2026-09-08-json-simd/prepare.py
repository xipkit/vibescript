"""Create JSON scanning experiment overlays over the unmodified runtime."""

import json
from pathlib import Path
import re
import sys

here = Path(__file__).resolve().parent
root = here.parents[2]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
source = root / "internal/runtime/json.go"
original = source.read_text()


def declaration(text, prefix):
    start = text.index(prefix)
    end = text.index("\n}\n", start) + 3
    return text[start:end]


reference = declaration(original, "type jsonValueParser struct {")
for text in (original, (root / "internal/runtime/json_memory.go").read_text()):
    for match in re.finditer(r"^func \(p \*jsonValueParser\) \w+\(", text, re.M):
        reference += "\n" + declaration(text, match.group())
reference = reference.replace("jsonValueParser", "auditJSONReferenceParser")
reference += "\n" + declaration(original, "func appendJSONString(").replace("appendJSONString", "auditAppendJSONStringReference")
reference = '''package runtime

import (
    "fmt"
    "math/big"
    "strconv"
    "strings"
    "unicode/utf16"
    "unicode/utf8"
    "github.com/mgomes/vibescript/vibes/value"
)

''' + reference
(out / "reference_test.go").write_text(reference)

common = {
    str(root / "internal/runtime/json_audit_reference_test.go"): str(out / "reference_test.go"),
}
for name in ("kernel.go", "scalar.go", "kernel_test.go", "benchmark_test.go", "accounting_test.go"):
    fixture = here / f"{name}.txt"
    if fixture.exists():
        common[str(root / f"internal/runtime/json_audit_{name}")] = str(fixture)

parse_string = declaration(original, "func (p *jsonValueParser) parseString()")
parse_escaped = declaration(original, "func (p *jsonValueParser) parseEscapedContents(")
stringify = declaration(original, "func appendJSONString(")
parse_loop = "for p.pos < len(p.raw) {"
stringify_loop = "for i := 0; i < len(s); {"

# Continue the original loops after a short ASCII run or the first Unicode
# byte, retaining the current offsets and every existing accounting boundary.
parse_tail = "func (p *jsonValueParser) auditParseStringScalarTail(start int) (string, error) {\n\t"
parse_tail += parse_string[parse_string.index(parse_loop):]
escaped_tail = "func (p *jsonValueParser) auditParseEscapedContentsScalarTail(size int, b *strings.Builder) (int, error) {\n\t"
escaped_tail += parse_escaped[parse_escaped.index(parse_loop):]
stringify_tail = "func auditAppendJSONStringScalarTail(buf []byte, s string, state *jsonStringifyState, start, i int) ([]byte, error) {\n\t"
stringify_tail += 'const hexDigits = "0123456789abcdef"\n\t'
stringify_tail += stringify[stringify.index(stringify_loop):].replace(stringify_loop, "for i < len(s) {", 1)

# Keep the original signatures for strings that are unlikely to benefit from
# batching. Continuation helpers remain available after a long ASCII prefix.
parse_original = parse_string.replace("parseString()", "auditParseStringOriginal()", 1)
escaped_original = parse_escaped.replace("parseEscapedContents(", "auditParseEscapedContentsOriginal(", 1)
stringify_original = stringify.replace("appendJSONString(", "auditAppendJSONStringOriginal(", 1)

for variant in ("original", "scalar", "simd"):
    replacements = dict(common)
    if variant != "original":
        modified = original
        suffix = "SIMD" if variant == "simd" else "Scalar"
        insertion = '''
        n := auditJSONParseSpanSUFFIX(p.raw[p.pos:])
        p.pos += n
        if p.pos == len(p.raw) { break }
        if n < 16 || p.raw[p.pos] >= utf8.RuneSelf {
            return p.auditParseStringScalarTail(start)
        }
'''.replace("SUFFIX", suffix)
        changed = parse_string.replace(parse_loop, parse_loop + insertion, 1)
        entry = '''
    if len(p.raw)-p.pos-1 < 16 || auditJSONParseSpanSUFFIX(p.raw[p.pos+1:p.pos+17]) != 16 {
        return p.auditParseStringOriginal()
    }
'''.replace("SUFFIX", suffix)
        changed = changed.replace("{\n", "{\n" + entry, 1)
        modified = modified.replace(parse_string, changed, 1)

        # An escape is expected at entry. Process escapes in the original
        # branch and only invoke the span scanner for ordinary ASCII bytes.
        escaped_start = parse_loop + "\n\t\tc := p.raw[p.pos]"
        escaped_insertion = '''
        if c >= utf8.RuneSelf {
            return p.auditParseEscapedContentsScalarTail(size, b)
        }
        if c >= 0x20 && c != '"' && c != '\\\\' {
            n := auditJSONParseSpanSUFFIX(p.raw[p.pos:])
            if b != nil { b.WriteString(p.raw[p.pos:p.pos+n]) }
            size += n
            p.pos += n
            if p.pos == len(p.raw) { break }
            if n < 16 || p.raw[p.pos] >= utf8.RuneSelf {
                return p.auditParseEscapedContentsScalarTail(size, b)
            }
            c = p.raw[p.pos]
        }
'''.replace("SUFFIX", suffix)
        changed = parse_escaped.replace(escaped_start, escaped_start + escaped_insertion, 1)
        entry = '''
    if p.pos-start < 16 {
        return p.auditParseEscapedContentsOriginal(start, b)
    }
'''
        changed = changed.replace("{\n", "{\n" + entry, 1)
        modified = modified.replace(parse_escaped, changed, 1)

        insertion = '''
        n := auditJSONStringifySpanSUFFIX(s[i:])
        i += n
        if i == len(s) { break }
        if n < 16 || s[i] >= utf8.RuneSelf {
            return auditAppendJSONStringScalarTail(buf, s, state, start, i)
        }
'''.replace("SUFFIX", suffix)
        changed = stringify.replace(stringify_loop, stringify_loop + insertion, 1)
        entry = '''
    if len(s) < 16 || auditJSONStringifySpanSUFFIX(s[:16]) != 16 {
        return auditAppendJSONStringOriginal(buf, s, state)
    }
'''.replace("SUFFIX", suffix)
        changed = changed.replace("{\n", "{\n" + entry, 1)
        modified = modified.replace(stringify, changed, 1)
        modified += "\n" + parse_tail + "\n" + escaped_tail + "\n" + stringify_tail
        modified += "\n" + parse_original + "\n" + escaped_original + "\n" + stringify_original
        path = out / f"json_{variant}.go"
        path.write_text(modified)
        replacements[str(source)] = str(path)
    (out / f"{variant}.json").write_text(json.dumps({"Replace": replacements}, indent=2) + "\n")
print(out)
