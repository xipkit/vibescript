#!/usr/bin/env python3
"""Import the programs recorded from Go v0.70.0's test suite into tests/golden/replay.

The recording lives outside the repository: a copy of the reference with a
recorder patched into `Script.Call` and `Engine.Compile` ran Go's test suite,
and `build_fixtures.py` turned each unique call or compile into a fixture chunk
plus a metadata file with the Go test name. This keeps what a Rust-only replay
needs: each distinct program once, each distinct argument set once, and one
line per case naming them. The Go outcomes are not kept; the goldens record the
Rust observations instead (`scripts/golden.py --record --corpus replay`).
"""
import argparse
import glob
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from golden import REPLAY, canonical, write_jsonl  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent


def key(text):
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("fixtures", type=Path, nargs="?", default=ROOT / ".cache/claude-go-replay/fixtures",
                        help="directory with chunk-*.json and meta.json")
    args = parser.parse_args()
    meta = json.loads((args.fixtures / "meta.json").read_text())
    programs, inputs, cases = {}, {}, []
    for path in sorted(glob.glob(str(args.fixtures / "chunk-*.json"))):
        for fixture in json.loads(Path(path).read_text()):
            info = meta[fixture["name"]]
            program = key(fixture["source"])
            programs[program] = fixture["source"]
            case = {"id": fixture["name"], "test": info.get("test"), "program": program,
                    "function": fixture["function"], "strict_effects": fixture.get("strict_effects", False)}
            if fixture.get("snippet_entry"):
                case["snippet_entry"] = fixture["snippet_entry"]
            if fixture["function"] is not None:
                given = {name: fixture.get(name) or [] for name in ["args", "kwargs", "globals"]}
                if any(given.values()):
                    inputs_key = key(canonical(given))
                    inputs[inputs_key] = {name: value for name, value in given.items() if value}
                    case["inputs"] = inputs_key
                case.update(steps=fixture.get("steps"), memory=fixture.get("memory"),
                            recursion=fixture.get("recursion", 256))
                if fixture.get("allow_require"):
                    case["allow_require"] = True
                if info.get("quota"):
                    case["quota"] = True
            cases.append(case)
    order = lambda name: (name.rstrip("0123456789"), int(name[len(name.rstrip("0123456789")):]))  # noqa: E731
    write_jsonl(REPLAY / "programs.jsonl.gz", [{"id": k, "source": programs[k]} for k in sorted(programs)])
    write_jsonl(REPLAY / "inputs.jsonl.gz", [{"id": k, **inputs[k]} for k in sorted(inputs)])
    write_jsonl(REPLAY / "cases.jsonl.gz", sorted(cases, key=lambda case: order(case["id"])))
    print(f"{len(programs)} programs, {len(inputs)} argument sets, {len(cases)} cases")


if __name__ == "__main__":
    main()
