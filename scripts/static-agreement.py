#!/usr/bin/env python3
"""Compare the static checker's verdicts on the migrated corpora with what
the goldens record at runtime.

Reads the output of `examples/static_corpus --out FILE` and the goldens. A
case the checker accepts must not raise a type error at runtime, except at an
explicit narrowing point (`as`, `JSON.parse_as`, host entry); every runtime
type error the goldens record in a migrated case must be reported statically
or come from such a point. Uncaught errors come from the goldens' records;
`language` cases rescue their errors, so their expected values are scanned
for type-error messages.

    python3 scripts/static-agreement.py .cache/static/corpus.json
"""
import collections
import gzip
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / ".cache/migrate/work/full"
# Errors a narrowing point raises: casts, parse_as and typed host entry.
NARROWING = re.compile(r"cast value|cast type check|JSON\.parse_as value|type check failed")
# Messages of runtime type errors that a rescue turns into a value.
TYPE_MESSAGE = re.compile(
    r"unsupported .* operands|expected [^,]+, got |cannot index|index must be integer|"
    r"undefined method|unknown [a-z_]+ (method|property)|non-callable|no implicit conversion|"
    r"comparison of|unsupported unary|must be (a|an) ")


def read_jsonl(path):
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt", encoding="utf-8") as lines:
        return [json.loads(line) for line in lines if line.strip()]


def goldens(corpus):
    if corpus == "language":
        return {case["name"]: {"expected": case["expected"]} for case in json.loads((ROOT / "tests/language.json").read_text())}
    for name in (f"{corpus}.jsonl", f"{corpus}.jsonl.gz"):
        path = ROOT / "tests/golden" / name
        if path.exists():
            return {record["id"]: record for record in read_jsonl(path)}
    return {}


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from strings(item)


def main():
    result = json.loads(Path(sys.argv[1] if len(sys.argv) > 1 else ".cache/static/corpus.json").read_text())
    failures = result["failures"]
    summary = {}
    for corpus, failing in failures.items():
        index = json.loads((WORK / corpus / "index.json").read_text())
        report = {entry["file"]: entry for entry in json.loads((WORK / f"{corpus}.report.json").read_text())}
        records = goldens(corpus)
        cases = collections.defaultdict(list)
        for key, file in index.items():
            cases[key.split("::")[0]].append(file)
        if corpus == "replay":
            # Replay cases name the program they run.
            cases = collections.defaultdict(list)
            for case in read_jsonl(ROOT / "tests/golden/replay/cases.jsonl.gz"):
                if case.get("program") in index:
                    cases[case["id"]].append(index[case["program"]])
        entries = {}
        if corpus == "replay":
            for case in read_jsonl(ROOT / "tests/golden/replay/cases.jsonl.gz"):
                entries[case["id"]] = case.get("function", "run")
        counts = collections.Counter()
        examples = collections.defaultdict(list)
        for case, files in cases.items():
            if any(report.get(file, {}).get("diagnostics") for file in files):
                continue
            accepted = not any(file in failing for file in files)
            record = records.get(case)
            if record is None:
                continue
            error = record.get("error")
            messages = []
            if error and error.get("kind") in ("Type",) and error.get("phase") == "call":
                messages.append(error["message"])
            if corpus == "language":
                messages.extend(m for m in strings(record["expected"]) if TYPE_MESSAGE.search(m))
            if not messages:
                continue
            def entry(message):
                # The host's arguments to the entry function are checked when the call starts.
                match = re.match(r"argument (\w+) expected", message)
                if not match:
                    return False
                source = "".join((WORK / corpus / file).read_text() for file in files)
                function = entries.get(case, "run")
                return re.search(rf"def {re.escape(function)}\([^)]*\b{match.group(1)}:", source) is not None
            narrowing = all(NARROWING.search(m) or entry(m) for m in messages)
            if accepted and not narrowing:
                kind = "accepted but raised a type error"
            elif accepted:
                kind = "accepted; type error at a narrowing point"
            else:
                kind = "reported statically"
            counts[kind] += 1
            if len(examples[kind]) < 8:
                examples[kind].append({"case": case, "files": files, "messages": messages[:2]})
        summary[corpus] = {"counts": dict(counts), "examples": dict(examples)}
        print(f"{corpus:14} " + "  ".join(f"{k}: {v}" for k, v in sorted(counts.items())))
    out = Path(".cache/static/agreement.json")
    out.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"details in {out}")


if __name__ == "__main__":
    main()
