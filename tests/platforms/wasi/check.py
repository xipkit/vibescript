#!/usr/bin/env python3
"""Build and execute WASI language and filesystem witnesses in two hosts."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def fixtures(base, guest):
    allowed = base / "allowed"
    (allowed / "sub").mkdir(parents=True)
    (allowed / "folder.vibe").mkdir()
    for name, source in {
        "numbers.vibe": "def run; 7; end",
        "counter.vibe": "n=0; def increment; n+=1; end",
        "changed.vibe": "def run; 3; end",
        "sub/relative.vibe": "def run; require('../numbers').run(); end",
    }.items():
        (allowed / name).write_text(source + "\n")
    long = allowed.joinpath(*(["long-directory-component-012345678901234567890123456789"] * 12))
    long.mkdir(parents=True)
    (long / "numbers.vibe").write_text("def run; 7; end\n")
    (base / "outside.vibe").write_text("def run; 999; end\n")
    for directory, value in [
        ("allowed2", 999), ("moving", 7), ("replacement", 999), ("accounting", 7)
    ]:
        (base / directory).mkdir()
        (base / directory / "numbers.vibe").write_text(f"def run; {value}; end\n")
    for name, target in {
        "alias.vibe": "numbers.vibe",
        "absolute.vibe": guest + "/allowed/numbers.vibe",
        "escape.vibe": "../outside.vibe",
        "absolute_escape.vibe": guest + "/outside.vibe",
        "prefix_escape.vibe": guest + "/allowed2/numbers.vibe",
        "broken.vibe": "missing.vibe",
    }.items():
        (allowed / name).symlink_to(target)
    for name, target in {
        "root_alias": "allowed",
        "directory_alias": "allowed/sub",
        "root_loop_a": "root_loop_b",
        "root_loop_b": "root_loop_a",
    }.items():
        (base / name).symlink_to(target, target_is_directory=True)


def overlap_fixtures(base):
    root, other = base / "root", base / "other"
    (root / "real/sub").mkdir(parents=True)
    other.mkdir()
    (root / "real/numbers.vibe").write_text("def run; 7; end\n")
    (other / "numbers.vibe").write_text("def run; 999; end\n")
    (other / "only_other.vibe").write_text("def run; require('./numbers').run(); end\n")
    (root / "alias").symlink_to("real", target_is_directory=True)
    return root, other


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", default="node")
    parser.add_argument("--wasmtime", default="wasmtime")
    args = parser.parse_args()
    for binary in [args.node, args.wasmtime]:
        if shutil.which(binary) is None:
            parser.error(f"executable not found: {binary}")
    if os.environ.get("RUST_MIN_STACK"):
        parser.error("run with normal stack limits; unset RUST_MIN_STACK")
    out = ROOT / ".cache/wasi-tests"
    out.mkdir(parents=True, exist_ok=True)
    out = Path(tempfile.mkdtemp(prefix="run-", dir=out))
    env = {**os.environ, "CARGO_BUILD_JOBS": "1", "RUST_TEST_THREADS": "1"}
    report = {"status": "running", "checks": [], "directory": str(out)}

    def record():
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    def run(label, command):
        command = [str(part) for part in command]
        with (out / (label + ".stdout")).open("w") as stdout, (
            out / (label + ".stderr")
        ).open("w") as stderr:
            result = subprocess.run(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr)
        report["checks"].append({"name": label, "command": command, "exit_code": result.returncode})
        record()
        if result.returncode:
            raise RuntimeError(f"{label} failed; see {out}")
        print(f"{label}: passed", flush=True)
        return (out / (label + ".stdout")).read_text()

    def host(runtime, binary, fixture, guest, arguments):
        if runtime == "node":
            return [args.node, HERE / "run.mjs", binary, fixture, guest, *arguments]
        return [args.wasmtime, "run", "--dir", f"{fixture}::{guest}", binary, *arguments]

    try:
        print(f"WASI artifacts: {out}", flush=True)
        record()
        cargo = ROOT / "scripts/cargo"
        common = ["--locked", "--manifest-path", HERE / "Cargo.toml", "--target", "wasm32-wasip1"]
        run("build-witness", [cargo, "build", *common, "--bins"])
        binary = ROOT / "target/wasm32-wasip1/debug/vibescript-wasi-witness.wasm"
        report["witness_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        messages = run("build-tests", [cargo, "test", *common, "--no-run", "--message-format=json",
                                       "--test", "language", "--test", "collection_counts", "--test", "commands"])
        tests = {}
        for line in messages.splitlines():
            item = json.loads(line)
            if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test"):
                if item.get("executable"):
                    tests[item["target"]["name"]] = item["executable"]
        assert set(tests) == {"language", "collection_counts", "commands"}, tests
        counters = {}
        for runtime in ["node", "wasmtime"]:
            for depth, guest in [("shallow", "/sandbox"), ("deep", "/nested/sandbox")]:
                label = f"{runtime}-{depth}"
                fixture = out / label
                fixtures(fixture, guest)
                arguments = [guest + "/allowed"]
                if runtime == "wasmtime":
                    arguments.append("--deny-absolute-links")
                else:
                    arguments.append("--path-based-directories")
                output = run(label, host(runtime, binary, fixture, guest, arguments))
                row = json.loads(output)
                assert row["status"] == "passed"
                counters[label] = row["accounting"]
            root, other = overlap_fixtures(out / (runtime + "-overlap"))
            if runtime == "node":
                command = [args.node, HERE / "run-overlap.mjs", binary, root, other]
            else:
                command = [args.wasmtime, "run", "--dir", f"{root}::/sandbox",
                           "--dir", f"{other}::/sandbox/real", binary, "--overlap"]
            run(runtime + "-overlap", command)
            for name, executable in tests.items():
                run(runtime + "-" + name, host(runtime, executable, out, "/sandbox", ["--test-threads=1"]))
        for depth in ["shallow", "deep"]:
            assert counters["node-" + depth] == counters["wasmtime-" + depth], counters
        report.update(status="passed", module_accounting=counters)
        record()
    except BaseException as error:
        report.update(status="failed", error=repr(error))
        record()
        raise


if __name__ == "__main__":
    main()
