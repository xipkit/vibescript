#!/usr/bin/env python3
"""Group the migrated sources that do not type check by the cause of their errors.

Reads what `examples/static_corpus --out FILE` wrote for a migrated tree and
counts, per corpus, the failing sources by cause, with examples. A source
with errors of several causes counts once under each.

    python3 scripts/static-causes.py .cache/migrate/work/full/static-automatic.json
    python3 scripts/static-causes.py FILE --examples 5 --corpus replay
"""
import argparse
import collections
import json
import re
from pathlib import Path

WORK = Path(__file__).resolve().parent.parent / ".cache/migrate/work/full"


def authored_result(original, function):
    """Whether the original source declared the result of `function`."""
    name = re.escape(re.split(r"[#.]", function)[-1])
    return re.search(rf"\bdef\s+(?:self\.)?{name}\b[^\n;]*->", original) is not None


def authored_param(original, function, param):
    """Whether the original source annotated parameter `param` of `function`."""
    name = re.escape(re.split(r"[#.]", function)[-1])
    return re.search(rf"\bdef\s+(?:self\.)?{name}\b[^\n;]*\b{re.escape(param)}\s*:[^:=]", original) is not None


def authored_ivar(original, ivar):
    """Whether the original source typed the property or instance variable `ivar`."""
    name = re.escape(ivar)
    return re.search(rf"(@{name}\s*:\s*\w|\b(property|getter|setter)\b[^\n;]*\b{name}\s*:\s*\w)", original) is not None


def cause(code, message, defined, original):
    """The cause of one error, from its code and message."""
    if code in ("V0301", "V0302", "V0303", "V0304", "V0305", "V0306"):
        return "call outside the signature table: arity, keywords or blocks the runtime tolerates"
    if code == "V0108" and re.search(r"`\+` is not defined for (string and|\S+ and string)", message):
        return "`+` joining a string with a value that does not convert"
    if code == "V0108":
        return "operator outside the signature table"
    if code == "V0115":
        return "generic bound the element type does not meet"
    if code == "V0106":
        return "`any` not narrowed (host globals, capabilities, parsed JSON)"
    if code == "V0107":
        if "is not defined for nil" in message and re.search(r"`(push|pop|shift|unshift|insert|concat|<<|delete\w*|clear|\w+=)`", message):
            return "`x[i]` mutated in place, which `fetch` would copy"
        return "`T?` used where the runs saw nil or could not be rerun"
    if code == "V0111":
        return "record indexed with a computed key"
    if code in ("V0110",):
        return "record read at a key it does not have"
    if code in ("V0204", "V0205"):
        return "instance variable not assigned on every path through initialize"
    if code == "V0102":
        return "local assigned values of two types"
    if code == "V0103":
        return "`nil`, `[]` or `{}` without a declared type"
    if code in ("V0104", "V0105"):
        return "condition that is not a bool"
    if code == "V0203":
        if re.search(r"has no member `[A-Za-z_?!]+=`", message):
            return "assignment to a namespace or a missing setter"
        return "member the type does not have"
    if code == "V0201":
        return "name the program does not define (host names, attr_* and the like)"
    if code in ("V0112", "V0113"):
        return "index of a value that cannot be indexed"
    if code == "V0310":
        return "call of a value"
    if code == "V0116":
        return "annotation naming an unknown type"
    if code == "V0117":
        return "value returned from a function without a result type"
    if code.startswith("V04"):
        return "removed spelling the migration left for a person"
    if code == "V0309":
        return "dynamic require"
    if code == "V0101":
        match = re.match(r"(?:argument \d+ \(`(\w+)`\)|keyword `(\w+):`) of `([^`]+)`", message)
        if match:
            function = match.group(3)
            if re.split(r"[#.]", function)[-1] in defined:
                if authored_param(original, function, match.group(1) or match.group(2)):
                    return "argument of a type the author's parameter annotation refuses"
                return "argument of a type the migration's parameter annotation refuses"
            return "builtin argument of a type the signature table refuses"
        if "of type symbol" in message or "is string, found symbol" in message:
            return "symbol where a string key is expected"
        match = re.match(r"`([^`]+)` returns", message)
        if match:
            if authored_result(original, match.group(1)):
                return "result the author's annotation refuses"
            return "result the migration's annotation refuses"
        if message.startswith("elements here"):
            return "element a collection's declared or inferred type refuses"
        if message.startswith("the block returns"):
            return "block result the signature table refuses"
        match = re.match(r"`@(\w+)` is", message)
        if match:
            if authored_ivar(original, match.group(1)):
                return "instance variable assigned a type the author's declaration refuses"
            return "instance variable assigned a type the migration's declaration refuses"
        if message.startswith("the annotation says"):
            return "value an author's annotation refuses"
        return "other type mismatch"
    return f"other: {code}"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("result", type=Path, help="the JSON static_corpus --out wrote")
    parser.add_argument("--work", type=Path, default=WORK, help="the migrated tree it checked")
    parser.add_argument("--export", type=Path,
                        help="the original sources, to tell the author's annotations from the migration's "
                             "(default: WORK/../export)")
    parser.add_argument("--corpus", help="comma-separated corpora (default: every one)")
    parser.add_argument("--examples", type=int, default=3)
    args = parser.parse_args()
    failures = json.loads(args.result.read_text())["failures"]
    export = args.export or args.work.parent / "export"
    wanted = set(args.corpus.split(",")) if args.corpus else None
    for corpus, files in failures.items():
        if wanted and corpus not in wanted:
            continue
        counts = collections.Counter()
        examples = collections.defaultdict(list)
        for file, errors in sorted(files.items()):
            path = args.work / corpus / file
            source = path.read_text(errors="replace") if path.exists() else ""
            defined = set(re.findall(r"\bdef\s+(?:self\.)?([A-Za-z_]\w*[?!=]?)", source))
            original_path = export / corpus / file
            original = original_path.read_text(errors="replace") if original_path.exists() else ""
            causes = set()
            for code, line, column, message in errors:
                why = cause(code, message, defined, original)
                if why not in causes and len(examples[why]) < args.examples:
                    examples[why].append(f"{file}:{line}:{column}: {message[:160]}")
                causes.add(why)
            counts.update(causes)
        print(f"== {corpus}: {len(files)} failing sources")
        for why, count in counts.most_common():
            print(f"  {count:7}  {why}")
            for example in examples[why]:
                print(f"             {example}")


if __name__ == "__main__":
    main()
