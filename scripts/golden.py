#!/usr/bin/env python3
"""Check a build against the golden corpora, or record them.

Each corpus pairs case sources, keyed by a stable id, with the observations
recorded from the Rust implementation: a value, output streams or an error with
its position. Accounting counters are recorded separately. A run executes every
case and fails on any observable difference; counter drift is reported but only
fails with --strict-counters. Nothing here needs Go. See tests/golden/README.md.
"""
import argparse
import collections
import functools
import gzip
import hashlib
import importlib.util
import json
import os
import random
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixtures  # noqa: E402
from block_fixtures import cases as block_cases  # noqa: E402
from capability_fixtures import cases as capability_cases  # noqa: E402
from module_fixtures import cases as module_cases  # noqa: E402
from signature_fixtures import cases as signature_cases  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
GOLDEN = ROOT / "tests/golden"
SCRATCH = ROOT / ".cache/golden"
CARGO = ROOT / "scripts/cargo"
HARNESS = ROOT / "target/gate/examples/golden"
VIBES = ROOT / "target/gate/vibes"
# Values, messages and streams longer than this are recorded as digests.
LIMIT = 4096
# A case that is quiet for this long is killed and recorded as hung.
HANG_SECONDS = 120
# Exhausting these limits depends on accounting, which may drift.
QUOTA_KINDS = {"Steps", "Memory"}
ZONES = ROOT / "src/time/zone/data/zoneinfo.zip"
# The Go results that the corpora started from were recorded in this zone.
LOCAL_ZONE = "America/Detroit"
CASE_FIELDS = ["function", "args", "globals", "strict_effects", "entropy_byte", "module_development",
               "module_allow", "module_deny", "allow_require", "capability_probe", "block_probe",
               "signature_probe", "notifications", "accounting", "steps", "stdout", "stderr"]


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def digest(data):
    return {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}


def read_jsonl(path):
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt", encoding="utf-8") as lines:
        return [json.loads(line) for line in lines if line.strip()]


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    text = "".join(canonical(record) + "\n" for record in records)
    if path.suffix == ".gz":
        # A fixed mtime keeps re-recorded files byte-identical when nothing changed.
        with open(path, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as out:
            out.write(text.encode())
    else:
        path.write_text(text, encoding="utf-8")


def function(body):
    return "def run(input)\n" + body + "\nend"


def read_source(path):
    """Reads a source file exactly, without translating line endings."""
    with open(path, encoding="utf-8", errors="replace", newline="") as file:
        return file.read()


def write_source(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as file:
        file.write(text)


# Corpus definitions ---------------------------------------------------------


def engine_case(cid, case):
    """Builds a harness case from a fixture-generator case."""
    out = {"id": cid, "source": case.get("source") or function(case["body"]), "function": case.get("function", "run")}
    for field in CASE_FIELDS:
        if field in case:
            out[field] = case[field]
    if "static_error" in case:
        out["_static_error"] = case["static_error"]
    if "args" not in case:
        out["args"] = [] if out["function"] == "__main__" else [None]
    if case.get("files") is not None:
        out["_files"] = case["files"]
    return out


def expect(out, case):
    """Attaches a generator's independent expectation for cross-checking."""
    if "expected" in case:
        out["_expected"] = case["expected"]
        out["_encoding"] = case.get("result_encoding", "")
        if out["_encoding"] != "typed":
            out["json"] = True
    for stream in ["stdout", "stderr"]:
        if case.get(stream) and stream + "_hex" in case:
            out["_" + stream] = case[stream + "_hex"]
    return out


def conformance_cases():
    cases = []
    for case in fixtures.conformance_cases(include_language=False) + fixtures.benchmark_cases():
        cases.append(expect(engine_case(case["name"], case), case))
    return cases


def language_cases():
    cases = []
    for case in json.loads((ROOT / "tests/language.json").read_text()):
        out = engine_case(case["name"], {k: v for k, v in case.items() if k != "args"})
        out = expect(out, case)
        out["json"] = True
        cases.append(out)
    return cases


# Reference rejections that ADR-007 syntax accepts, as tests/commands.rs lists
# them: a class variable declaration in a module body.
ACCEPTED_SYNTAX = {"module_edge_16"}


def rejection_cases():
    cases = []
    for filename in ["language-errors.json", "syntax-errors.json"]:
        for case in json.loads((ROOT / "tests" / filename).read_text()):
            out = engine_case(case["name"], case)
            # A case the static checker rejects keeps its recorded outcome without static types.
            if case["name"] not in ACCEPTED_SYNTAX and "static_error" not in case:
                out["_phase"] = "compile" if filename == "syntax-errors.json" else "call"
            cases.append(out)
    return cases


def compatibility_cases():
    cases = []
    for case in json.loads((ROOT / "docs/compatibility-cases.json").read_text()):
        cid = "compatibility-cases/" + case["name"]
        cases.append(engine_case(cid, case))
        if "source_with_alias" in case:
            cases.append(engine_case(cid + "/with_alias", {**case, "source": case["source_with_alias"]}))
    policy = fixtures.host_global_cases() + module_cases() + capability_cases() + block_cases() + signature_cases()
    for case in policy:
        if "policy" in case:
            cases.append(engine_case("policies/" + case["name"], case))
    records = {
        "forwarding-differences": ["cases"],
        "hash-new-differences": ["diagnostics", "intentional"],
        "loop-differences": ["intentional"],
        "options-hash-differences": ["cases", "resolved_type_diagnostics"],
        "value-helper-differences": None,
        "computed-call-gaps": None,
    }
    for name, sections in records.items():
        data = json.loads((ROOT / "docs" / f"{name}.json").read_text())
        entries = data if sections is None else [entry for section in sections for entry in data[section]]
        for entry in entries:
            if "source" in entry:
                cases.append(engine_case(f"{name}/{entry['name']}", entry))
    return cases


REPLAY = GOLDEN / "replay"


def replay_cases():
    programs = {record["id"]: record["source"] for record in read_jsonl(REPLAY / "programs.jsonl.gz")}
    inputs = {record["id"]: {"input": record["id"], "typed_args": record.get("args", []),
                             "typed_kwargs": record.get("kwargs", []), "typed_globals": record.get("globals", [])}
              for record in read_jsonl(REPLAY / "inputs.jsonl.gz")}
    cases = []
    for spec in read_jsonl(REPLAY / "cases.jsonl.gz"):
        out = {"id": spec["id"], "source": programs[spec["program"]], "function": spec.get("function"),
               "stdout": True, "stderr": True, "_source_key": spec["program"], "_quota": spec.get("quota", False)}
        for field in ["strict_effects", "allow_require", "steps", "memory", "recursion"]:
            if field in spec:
                out[field] = spec[field]
        if spec.get("snippet_entry") and spec.get("snippet_entry") == out["function"]:
            out["function"] = "__main__"
        if "inputs" in spec:
            out["inputs"] = spec["inputs"]
            out["_input"] = inputs[spec["inputs"]]
        if "static_error" in spec:
            out["_static_error"] = spec["static_error"]
        cases.append(out)
    return cases


@functools.cache
def load_script(name):
    """Imports a hyphenated script from scripts/ as a module."""
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PARSE_SOURCES = ["tests/site", "tests/upstream"]


def parse_cases():
    """Mutated corpus programs, each compiled only, as `scripts/parse-sweep.py` makes them."""
    sweep = load_script("parse-sweep")
    rng = random.Random(1)
    cases, seen = [], set()
    for path in sorted(p for d in PARSE_SOURCES for p in (ROOT / d).rglob("*.vibe")):
        source = read_source(path)
        name = path.relative_to(ROOT).as_posix()
        for kind, text in sweep.mutations(source, rng, 8):
            key = hashlib.sha1(text.encode()).hexdigest()
            if key not in seen:
                seen.add(key)
                cases.append({"id": f"{name}:{kind}:{key[:12]}", "source": text, "function": None})
    return cases


# Command-line corpus --------------------------------------------------------

CLI_ARGUMENTS = [
    [], ["--help"], ["-h"], ["-h", "-x"], ["--help", "run"], ["help"], ["h"],
    ["help", "run"], ["help", "fmt"], ["help", "analyze"], ["help", "test"],
    ["help", "lsp"], ["help", "repl"], ["help", "help"], ["help", "h"],
    ["help", "nope"], ["help", ""], ["help", "run", "extra"], ["help", "run", "--"],
    ["help", "--", "run"], ["help", "-h", "run"], ["help", "-x"], ["h", "--help"],
    ["run", "-h"], ["fmt", "--help"], ["analyze", "--help"], ["test", "--help"],
    ["lsp", "--help"], ["repl", "--help"], ["unknown"], ["unknown", "--help"],
    ["unknown", "", "--help"], ["--", "run"], ["-help"], ["--h"], ["-h="],
    ["--help=false"], [""], [" run"], ["RUN"], ["-"], ["-x"], ["--bogus"], ["---x"],
    ["-=x"], ["-x=1"], ["-1"], ["-1a"], ["--1"], ["-é"], ["-_"], ["-version"],
    ["run", "-unknown"], ["fmt", "--unknown=value"], ["run", "--e"], ["fmt", "---w"],
    ["run", "--=value"], ["run", "-e="], ["run", "-e", "   "], ["run"], ["run", "-watch"],
    ["run", "-module-path"], ["run", "-module-path", "/nonexistent", "-e", "1"],
    ["run", "/nonexistent.vibe"], ["run", "-e", "1", "-watch"], ["run", "-e", "1", "extra"],
    ["run", "-function", "f", "-e", "1"], ["run", "-check=yes", "-e", "1"],
    ["run", "-step-quota=nope", "-unknown"], ["run", "-step-quota", "99999999999999999999"],
    ["run", "-profile", "nope", "-e", "1"], ["run", "--help=1"], ["run", "-x", "-h"],
    ["run", "-e", "1 + 2"], ["run", "-e", "[1, nil, 2.0, 1e20, :s, {a: [nil]}]"],
    ["run", "-check", "-e", "1"],
    ["check"], ["check", "a.vibe", "b.vibe"], ["check", "/nonexistent.vibe"],
    ["fmt"], ["fmt", "-"], ["fmt", "/nonexistent"], ["analyze"], ["analyze", "-x"],
    ["analyze", "a", "b"], ["test", "-run"], ["test", "/nonexistent"],
    ["test", "-profile", "nope"], ["lsp", "extra"], ["repl", "extra"],
    ["repl", "-profile", "nope"],
]
CLI_SOURCES = ["tests/site", "tests/upstream", "examples"]
CLI_SUITE = {
    "math_test.vibe": "def test_addition\n  assert 1 + 2 == 3\nend\n\ndef helper -> int\n  1\nend\n",
    "broken_test.vibe": "def test_failure\n  assert 1 == 2, \"one is not two\"\nend\n\ndef test_needs(value: int)\nend\n\n"
                        "def test_prints\n  puts \"hello\"\n  raise \"boom\"\nend\n",
    "top_test.vibe": "puts \"top\"\ndef test_a\nend\n",
    "assign_test.vibe": "x = 1\n",
    "empty_test.vibe": "def helper\nend\n",
    "nested/deep_test.vibe": "def test_div\n  1 / 0\nend\n",
}


def cli_programs():
    return sorted(p.relative_to(ROOT).as_posix() for d in CLI_SOURCES for p in (ROOT / d).rglob("*.vibe"))


def cli_whitespace():
    """Generated whitespace-only and near-empty files for the formatter."""
    rng = random.Random(3)
    pieces = [" ", "\t", "\r", "\n", "\r\n", "  \t", "x", "#", "é", "\n \n"]
    files = {}
    for index in range(2000):
        text = "".join(rng.choice(pieces) for _ in range(rng.randint(0, 30)))
        files[f"random/r{index}.vibe"] = text.encode().replace(b"#", b"\xff", 1)
    return files


def cli_cases():
    cases = [{"id": "args:" + canonical(args), "args": args, "cwd": "empty"} for args in CLI_ARGUMENTS]
    for name in cli_programs():
        for command in ["run", "analyze", "fmt"]:
            cases.append({"id": f"{command}:{name}", "args": [command, name], "cwd": "corpus"})
    for name in cli_whitespace():
        cases.append({"id": f"fmt:{name}", "args": ["fmt", name], "cwd": "whitespace"})
    for flags in [[], ["-check"]]:
        for tree in ["corpus", "whitespace"]:
            cases.append({"id": f"fmt-tree:{' '.join(flags + [tree])}", "args": ["fmt", *flags, "."], "cwd": tree})
    cases.append({"id": "fmt-tree:-w corpus", "args": ["fmt", "-w", "."], "cwd": "corpus", "copy": True})
    cases.append({"id": "fmt-tree:-w whitespace", "args": ["fmt", "-w", "."], "cwd": "whitespace", "copy": True})
    for args in [["test"], ["test", "-run", "fail|add", "."], ["test", "empty_test.vibe"]]:
        cases.append({"id": "test:" + " ".join(args[1:]), "args": args, "cwd": "suite"})
    return cases


def cli_tree(base):
    for name in cli_programs():
        target = base / "corpus" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, target)
    for name, data in cli_whitespace().items():
        target = base / "whitespace" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    for name, text in CLI_SUITE.items():
        target = base / "suite" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    (base / "empty").mkdir()


def tree_digest(directory):
    hasher = hashlib.sha256()
    for path in sorted(p for p in directory.rglob("*") if p.is_file()):
        hasher.update(path.relative_to(directory).as_posix().encode() + b"\0")
        hasher.update(hashlib.sha256(path.read_bytes()).digest())
    return hasher.hexdigest()


def run_cli(binary, cases, jobs):
    with tempfile.TemporaryDirectory(prefix="cli-", dir=scratch()) as base:
        base = Path(base)
        cli_tree(base)
        # Every command runs in a directory just below the tree.
        env = pinned_zones(base, "../zoneinfo.zip")
        # Messages that name the working directory must not depend on where it is.
        spellings = sorted({str(base).encode(), os.path.realpath(base).encode()}, key=len, reverse=True)

        def placed(data):
            for spelling in spellings:
                data = data.replace(spelling, b"$TREE")
            return data

        def one(case):
            cwd = base / case["cwd"]
            if case.get("copy"):
                cwd = base / ("copy-" + case["cwd"])
                shutil.copytree(base / case["cwd"], cwd)
            try:
                done = subprocess.run([str(binary), *case["args"]], cwd=cwd, stdin=subprocess.DEVNULL,
                                      capture_output=True, timeout=60, env=env)
                record = {"status": done.returncode, "stdout": placed(done.stdout).hex(),
                          "stderr": placed(done.stderr).hex()}
            except subprocess.TimeoutExpired:
                record = {"status": "timeout", "stdout": "", "stderr": ""}
            if case.get("copy"):
                record["tree"] = tree_digest(cwd)
                shutil.rmtree(cwd)
            return case["id"], record

        with ThreadPoolExecutor(jobs) as pool:
            return dict(pool.map(one, cases))


def cli_record(observation):
    record = {"status": observation["status"]}
    for stream in ["stdout", "stderr"]:
        if observation[stream]:
            record[stream] = stream_record(observation[stream])
    if "tree" in observation:
        record["tree"] = observation["tree"]
    return record


# Language server corpus -----------------------------------------------------

LSP_SOURCES = ["tests/site", "tests/upstream/examples", "tests/lsp"]


def lsp_documents():
    return sorted(p.relative_to(ROOT).as_posix() for d in LSP_SOURCES for p in (ROOT / d).rglob("*.vibe"))


def lsp_cases():
    return [{"id": "protocol"}] + [{"id": document} for document in lsp_documents()]


class Server:
    """One `vibes lsp` process driven one message at a time."""

    def __init__(self, binary, directory, env):
        self.process = subprocess.Popen([str(binary), "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, cwd=directory, env=env)

    def read(self):
        length = None
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError("language server exited")
            line = line.strip()
            if not line:
                break
            name, _, value = line.partition(b":")
            if name.strip().lower() == b"content-length":
                length = int(value)
        return self.process.stdout.read(length)

    def exchange(self, message, expected):
        if isinstance(message, bytes):
            self.process.stdin.write(message)
        else:
            body = json.dumps(message, ensure_ascii=False).encode()
            self.process.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.process.stdin.flush()
        return [self.read() for _ in range(expected)]

    def close(self):
        try:
            self.exchange({"jsonrpc": "2.0", "method": "exit"}, 0)
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        self.process.wait(timeout=60)


ID_FIELD = re.compile(rb'^\{"jsonrpc":"2\.0","id":(?:-?[0-9.]+|null|"[^"]*"),')


def lsp_transcript(binary, case, directory, env):
    """Returns the reply bodies, with request ids removed, of one session."""
    transcripts = load_script("lsp-transcripts")
    server = Server(binary, directory, env)
    # A hung server is killed, which ends the session with an error.
    timer = threading.Timer(10 * HANG_SECONDS, server.process.kill)
    timer.start()
    replies = []

    def send(message, expected):
        replies.append(b"\n".join(ID_FIELD.sub(b'{"jsonrpc":"2.0",', reply, count=1)
                                   for reply in server.exchange(message, expected)))

    try:
        if case["id"] == "protocol":
            for message, expected in transcripts.PROTOCOL:
                send(message, expected)
            return replies
        send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}, 1)
        send({"jsonrpc": "2.0", "method": "initialized", "params": {}}, 0)
        uri = "file:///corpus/" + case["id"].replace(" ", "%20")
        request = 0
        for message, expected in transcripts.session(uri, read_source(ROOT / case["id"])):
            message = {"jsonrpc": "2.0", **message}
            if not message["method"].startswith("textDocument/did"):
                request += 1
                message["id"] = request
            send(message, expected)
        send({"jsonrpc": "2.0", "id": request + 1, "method": "shutdown"}, 1)
        return replies
    finally:
        timer.cancel()
        server.close()


def run_lsp(binary, cases, jobs):
    with tempfile.TemporaryDirectory(prefix="lsp-", dir=scratch()) as directory:
        env = pinned_zones(Path(directory), "zoneinfo.zip")

        def one(case):
            try:
                return case["id"], {"replies": lsp_transcript(binary, case, directory, env)}
            except Exception as error:  # noqa: BLE001 - reported as the observation
                return case["id"], {"failure": repr(error)}

        with ThreadPoolExecutor(jobs) as pool:
            return dict(pool.map(one, cases))


class Replies:
    """The table of distinct language-server replies that transcripts index."""

    def __init__(self, records=()):
        self.bodies = [record["reply"] for record in records]
        self.index = {body: i for i, body in enumerate(self.bodies)}

    def add(self, body):
        if body not in self.index:
            self.index[body] = len(self.bodies)
            self.bodies.append(body)
        return self.index[body]


def lsp_record(observation, table):
    if "failure" in observation:
        return {"failure": observation["failure"]}
    return {"replies": [table.add(body.decode("utf-8", "replace")) for body in observation["replies"]]}


# Engine execution -----------------------------------------------------------


def scratch():
    SCRATCH.mkdir(parents=True, exist_ok=True)
    return SCRATCH


def pinned_zones(directory, zoneinfo):
    """Returns an environment whose local and named time zones come from the bundled
    database, so results do not depend on the host's zone or tz data.

    The engine reads named zones from ZONEINFO and charges for its length, so
    `zoneinfo` is a fixed path relative to where the process runs.
    """
    shutil.copyfile(ZONES, directory / "zoneinfo.zip")
    with zipfile.ZipFile(ZONES) as archive:
        (directory / "localtime").write_bytes(archive.read(LOCAL_ZONE))
    return {**os.environ, "TZ": str(directory / "localtime"), "ZONEINFO": zoneinfo}


def run_chunk(harness, path, ids, env):
    """Runs one case file, resuming after a crash or hang; returns id -> observation."""
    observations = {}
    start = 0
    while start < len(ids):
        with tempfile.TemporaryFile() as errors:
            process = subprocess.Popen([str(harness), str(path), str(start)], stdout=subprocess.PIPE, stderr=errors,
                                       cwd=path.parent, env=env)
            progress = [time.monotonic()]
            finished = threading.Event()
            hung = []

            def watch():
                while not finished.wait(1):
                    if time.monotonic() - progress[0] > HANG_SECONDS:
                        hung.append(True)
                        process.kill()
                        return

            threading.Thread(target=watch, daemon=True).start()
            count = 0
            for line in process.stdout:
                progress[0] = time.monotonic()
                observation = json.loads(line)
                observations[observation["id"]] = observation
                count += 1
            process.wait()
            finished.set()
            errors.seek(0)
            tail = errors.read()[-2000:].decode("utf-8", "replace")
        start += count
        if start < len(ids):
            phase = "hang" if hung else "crash"
            observations[ids[start]] = {"id": ids[start], "phase": phase,
                                        "error": {"message": f"exit {process.returncode}: {tail}"}}
            start += 1
    return observations


def run_engine(harness, cases, jobs, label):
    """Runs cases through the harness in parallel; returns id -> observation."""
    with tempfile.TemporaryDirectory(prefix=f"{label}-", dir=scratch()) as directory:
        directory = Path(directory)
        env = pinned_zones(directory, "zoneinfo.zip")
        chunks, current, size, defined = [], [], 0, set()
        for index, case in enumerate(cases):
            given = case.get("_input")
            case = {k: v for k, v in case.items() if not k.startswith("_") or k == "_files"}
            files = case.pop("_files", None)
            if files is not None:
                root = directory / "modules" / f"{index:07}"
                root.mkdir(parents=True)
                for name, text in files.items():
                    relative = Path(name)
                    if relative.is_absolute() or ".." in relative.parts:
                        raise ValueError(f"{case['id']}: invalid module path {name!r}")
                    write_source(root / relative, text)
                case["module_paths"] = [str(root.relative_to(directory))]
            line = json.dumps(case, ensure_ascii=False)
            if current and (len(current) >= 200 or size + len(line) > 8_000_000):
                chunks.append(current)
                current, size, defined = [], 0, set()
            if given is not None and given["input"] not in defined:
                # Shared argument sets are written once per chunk, before their first use.
                definition = json.dumps(given, ensure_ascii=False)
                defined.add(given["input"])
                current.append((None, definition))
                size += len(definition)
            current.append((case["id"], line))
            size += len(line)
        if current:
            chunks.append(current)
        paths = []
        for number, chunk in enumerate(chunks):
            path = directory / f"chunk-{number:05}.jsonl"
            path.write_text("".join(line + "\n" for _, line in chunk), encoding="utf-8")
            paths.append((path, [cid for cid, _ in chunk if cid is not None]))
        observations = {}
        # The largest chunks first, so a slow tail does not wait on one worker.
        paths.sort(key=lambda item: -item[0].stat().st_size)
        with ThreadPoolExecutor(jobs) as pool:
            for result in pool.map(lambda item: run_chunk(harness, *item, env), paths):
                observations.update(result)
        return observations


def compact_text(fields, name, text):
    data = text.encode("utf-8")
    if len(data) > LIMIT:
        fields[name + "_digest"] = digest(data)
    else:
        fields[name] = text


def stream_record(hexdata):
    data = bytes.fromhex(hexdata)
    if len(data) > LIMIT:
        return digest(data)
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return {"hex": hexdata}


def engine_record(observation):
    """The golden form of a harness observation, and its counters if it succeeded."""
    record, counters = {}, None
    phase = observation["phase"]
    if phase == "ok":
        value = observation["value"]
        text = canonical(value).encode("utf-8")
        record["ok"] = digest(text) if len(text) > LIMIT else value
        counters = [observation["steps"], observation["peak"], observation["retained"]]
    elif phase == "compiled":
        record["compiled"] = True
    else:
        error = {"phase": phase}
        detail = observation.get("error", {})
        for field in ["kind", "class", "at", "message_hex"]:
            if field in detail:
                error[field] = detail[field]
        if "message" in detail:
            compact_text(error, "message", detail["message"])
        record["error"] = error
    for stream in ["stdout", "stderr"]:
        if observation.get(stream):
            record[stream] = stream_record(observation[stream])
    return record, counters


def outcome_class(record):
    if "error" in record:
        return "error:" + record["error"].get("kind", record["error"]["phase"])
    if "status" in record:
        return f"status:{record['status']}"
    return "compiled" if record.get("compiled") else "ok"


def same_json(a, b):
    """JSON equality that keeps booleans, integers and floats distinct."""
    if isinstance(a, (bool, int, float)) or isinstance(b, (bool, int, float)):
        return type(a) is type(b) and a == b
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same_json(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(same_json(a[k], b[k]) for k in a)
    return a == b


def static_error_failure(case, observation):
    """Checks a case the static checker must reject; returns a message or None."""
    expected = case["_static_error"]
    error = observation.get("error", {})
    got = {"phase": observation["phase"], "code": error.get("code"), "at": error.get("at")}
    want = {"phase": "compile", "code": expected["code"], "at": expected["at"]}
    if got != want:
        return f"expected a static {want['code']} error at {want['at']}, got {canonical(got)}: {error.get('message', '')[:200]}"
    return None


def expectation_failure(case, observation):
    """Checks a generator's independent expectation; returns a message or None."""
    if "_static_error" in case:
        return static_error_failure(case, observation)
    if "_phase" in case and observation["phase"] != case["_phase"]:
        return f"expected a {case['_phase']} error, got {observation['phase']}"
    if "_expected" not in case:
        return None
    if observation["phase"] != "ok":
        return f"expected a value, got {observation['phase']}: {canonical(observation.get('error'))[:300]}"
    if case.get("_encoding") == "typed":
        if not same_json(["typed-v1", observation["value"]], case["_expected"]):
            return f"expected {canonical(case['_expected'])[:300]}, got {canonical(observation['value'])[:300]}"
    elif "json" not in observation:
        return f"result has no JSON form: {observation.get('json_error')}"
    elif not same_json(json.loads(observation["json"]), case["_expected"]):
        return f"expected {canonical(case['_expected'])[:300]}, got {observation['json'][:300]}"
    for stream in ["stdout", "stderr"]:
        if "_" + stream in case and observation.get(stream, "") != case["_" + stream]:
            shown = bytes.fromhex(observation.get(stream, ""))[:300]
            return f"{stream} differs from the expectation: got {shown!r}"
    return None


# Corpus registry ------------------------------------------------------------


class Corpus:
    def __init__(self, name, kind, cases, description, compress=False, legacy=False, migratable=True):
        self.name, self.kind, self.cases, self.description = name, kind, cases, description
        self.legacy, self.migratable = legacy, migratable and kind == "engine"
        suffix = ".jsonl.gz" if compress else ".jsonl"
        self.golden = GOLDEN / f"{name}{suffix}"
        self.counters = GOLDEN / f"{name}.counters.jsonl.gz"


CORPORA = {corpus.name: corpus for corpus in [
    Corpus("conformance", "engine", conformance_cases,
           "generated conformance, site, upstream, host-binding and benchmark cases (scripts/fixtures.py)"),
    Corpus("language", "engine", language_cases,
           "tests/language.json, whose expected values and output are the goldens", legacy=True),
    Corpus("rejections", "engine", rejection_cases,
           "runtime and syntax rejections (tests/language-errors.json, tests/syntax-errors.json)", compress=True),
    Corpus("compatibility", "engine", compatibility_cases,
           "selected differences from Go v0.70.0 (docs/*-differences.json, docs/compatibility-cases.json)"),
    Corpus("replay", "engine", replay_cases,
           "calls and compiles recorded from Go v0.70.0's test suite (tests/golden/replay)", compress=True),
    Corpus("parse", "engine", parse_cases,
           "mutated site and upstream programs, compiled only (as in scripts/parse-sweep.py)", compress=True,
           migratable=False),
    Corpus("cli", "cli", cli_cases, "vibes commands over the corpus programs (as in scripts/compare-cli.py)",
           compress=True),
    Corpus("lsp", "lsp", lsp_cases, "vibes lsp sessions over the corpus programs (as in scripts/lsp-transcripts.py)",
           compress=True),
]}


# Source overrides for migrated corpora -------------------------------------


def source_keys(case):
    """Yields (key, text) for every source a case compiles or requires."""
    key = case.get("_source_key", case["id"])
    yield key, case["source"]
    for name, text in (case.get("_files") or {}).items():
        yield f"{key}::{name}", text


def apply_overrides(cases, overrides):
    """Replaces case sources with migrated ones; marks the cases that changed."""
    for case in cases:
        key = case.get("_source_key", case["id"])
        changed = False
        if key in overrides:
            changed |= overrides[key] != case["source"]
            case["source"] = overrides[key]
        if case.get("_files"):
            files = dict(case["_files"])
            for name in files:
                if f"{key}::{name}" in overrides:
                    changed |= overrides[f"{key}::{name}"] != files[name]
                    files[name] = overrides[f"{key}::{name}"]
            case["_files"] = files
        case["_overridden"] = changed


def export_path(key):
    path = re.sub(r"[^A-Za-z0-9._/-]", "_", key.replace("::", ".files/"))
    parts = [part if part not in ("", ".", "..") else "_" for part in path.split("/")]
    path = "/".join(parts)
    return path if path.endswith(".vibe") else path + ".vibe"


def case_inputs(case, index):
    """The invocation `vibes migrate --inputs` runs for a case, naming its exported source."""
    key = case.get("_source_key", case["id"])
    record = {"file": index[key]}
    for field in CASE_FIELDS + ["module_allow", "module_deny", "module_development", "allow_require",
                                "memory", "recursion"]:
        if field in case:
            record[field] = case[field]
    given = case.get("_input")
    if given is not None:
        for field in ["typed_args", "typed_kwargs", "typed_globals"]:
            if given.get(field):
                record[field] = given[field]
    if case.get("_files"):
        first = next(iter(case["_files"]))
        record["module_paths"] = [index[f"{key}::{first}"].rsplit("/", first.count("/") + 1)[0]]
    return record


def export(corpora, directory):
    for corpus in corpora:
        if not corpus.migratable:
            print(f"{corpus.name}: not migrated, so not exported")
            continue
        index, used, invocations = {}, set(), []
        cases = corpus.cases()
        for case in cases:
            for key, text in source_keys(case):
                if key in index:
                    continue
                path = export_path(key)
                while path.lower() in used:
                    path = path.removesuffix(".vibe") + "_.vibe"
                used.add(path.lower())
                write_source(directory / corpus.name / path, text)
                index[key] = path
        for case in cases:
            if case.get("function") is not None:
                invocations.append(case_inputs(case, index))
        (directory / corpus.name / "index.json").write_text(canonical(index) + "\n")
        write_jsonl(directory / corpus.name / "inputs.jsonl", invocations)
        print(f"{corpus.name}: exported {len(index)} sources and {len(invocations)} invocations "
              f"to {directory / corpus.name}")


def load_sources(directory):
    overrides = {}
    for index in sorted(Path(directory).glob("*/index.json")):
        paths = json.loads(index.read_text())
        overrides[index.parent.name] = {key: read_source(index.parent / path) for key, path in paths.items()}
    return overrides


# Checking and recording -----------------------------------------------------


class Report:
    def __init__(self, show):
        self.show, self.failed = show, False
        self.cases = {}

    def section(self, corpus, total, problems, notes):
        self.cases[corpus] = {"total": total, **{
            category: {"blocking": entry["blocking"], "cases": [cid for cid, _ in entry["cases"]]}
            for category, entry in problems.items() if entry["cases"]}}
        blocking = {k: v for k, v in problems.items() if v["blocking"]}
        count = sum(len(v["cases"]) for v in blocking.values())
        status = "FAILED" if count else "ok"
        print(f"{corpus}: {total} cases, {count} failures [{status}]", flush=True)
        for category, entry in problems.items():
            cases = entry["cases"]
            if not cases:
                continue
            tag = "" if entry["blocking"] else " (not blocking)"
            print(f"  {len(cases)} {category}{tag}")
            for cid, message in cases[:self.show]:
                print(f"    {cid}: {message}")
        for note in notes:
            print(f"  {note}")
        self.failed |= bool(count)


def readable(node):
    """Renders a typed-v1 node, or the digest standing for one, for reports."""
    if isinstance(node, dict):
        return f"<{node['bytes']} bytes, sha256 {node['sha256'][:12]}>"
    kind = node[0]
    text = lambda hexdata: bytes.fromhex(hexdata).decode("utf-8", "backslashreplace")  # noqa: E731
    if kind == "nil":
        return "nil"
    if kind == "bool":
        return "true" if node[1] else "false"
    if kind in ("int", "duration"):
        return node[1] + ("s" if kind == "duration" else "")
    if kind == "float":
        if node[1] == "nan":
            return "nan"
        value = struct.unpack(">d", bytes.fromhex(node[1]))[0]
        return repr(value) if value == value else f"nan (bits {node[1]})"
    if kind == "string":
        return json.dumps(text(node[1]), ensure_ascii=False)
    if kind == "symbol":
        return ":" + text(node[1])
    if kind == "money":
        return f"{node[2]} {node[1]} cents"
    if kind == "array":
        return "[" + ", ".join(readable(item) for item in node[1]) + "]"
    if kind in ("hash", "object"):
        return "{" + ", ".join(f"{json.dumps(text(key), ensure_ascii=False)}: {readable(item)}"
                               for key, item in node[1]) + "}"
    return canonical(node)


def describe(record):
    """Summarizes a golden record for reports."""
    if record is None:
        return "nothing"
    if "varies" in record:
        return f"a varying {record['varies']} outcome"
    parts = []
    if "ok" in record:
        parts.append("value " + readable(record["ok"]))
    elif record.get("compiled"):
        parts.append("compiled")
    elif "error" in record:
        error = record["error"]
        at = ":".join(map(str, error["at"])) if "at" in error else "?"
        message = error.get("message") or error.get("message_digest") or error.get("message_hex")
        parts.append(f"{error['phase']} error {error.get('kind')}/{error.get('class')} at {at}: {message}")
    elif "status" in record:
        parts.append(f"status {record['status']}")
    for field in ["stdout", "stderr", "tree", "replies", "failure"]:
        if field in record:
            value = record[field]
            parts.append(f"{field} " + (json.dumps(value, ensure_ascii=False) if isinstance(value, str) else canonical(value)))
    return "; ".join(parts)[:500]


def first_difference(expected, got, path="result"):
    """Finds where two typed-v1 nodes first differ."""
    if (isinstance(expected, list) and isinstance(got, list) and expected[:1] == got[:1]
            and expected[0] in ("array", "hash", "object") and len(expected[1]) == len(got[1])):
        for index, (a, b) in enumerate(zip(expected[1], got[1])):
            if a == b:
                continue
            if expected[0] == "array":
                return first_difference(a, b, f"{path}[{index}]")
            if a[0] != b[0]:
                return path, f"key {readable(['string', a[0]])}", f"key {readable(['string', b[0]])}"
            return first_difference(a[1], b[1], f"{path}[{readable(['string', a[0]])}]")
    return path, readable(expected), readable(got)


def difference(expected, got):
    """Explains how an observation differs from its golden."""
    if isinstance(expected.get("ok"), list) and isinstance(got.get("ok"), list) and expected["ok"] != got["ok"]:
        path, a, b = first_difference(expected["ok"], got["ok"])
        return f"at {path}: expected {a[:300]}, got {b[:300]}"
    return f"expected {describe(expected)}\n      got      {describe(got)}"


def counter_notes(expected, actual, show):
    changed = collections.Counter()
    examples = []
    for cid, counters in actual.items():
        before = expected.get(cid)
        # An empty entry marks counters that differed between recording runs.
        if not before or before == counters:
            continue
        for name, a, b in zip(["steps", "peak bytes", "retained bytes"], before, counters):
            if a != b:
                changed[name] += 1
        if len(examples) < show:
            examples.append(f"{cid}: {before} -> {counters}")
    missing = len(set(actual) - set(expected))
    if not changed and not missing:
        return [], False
    notes = ["counter drift: " + ", ".join(f"{n} {name}" for name, n in changed.items()) +
             (f", {missing} unrecorded" if missing else "")]
    notes += ["  " + example for example in examples]
    return notes, True


def load_counters(corpus):
    if not corpus.counters.exists():
        return {}
    return {record[0]: record[1:] for record in read_jsonl(corpus.counters)}


def check_corpus(corpus, args, overrides, report):
    """Runs one corpus and reports how it compares with its goldens, or records them."""
    started = time.monotonic()
    cases = corpus.cases()
    # The parse sweep records what parses, not what type checks.
    if corpus.name == "parse":
        for case in cases:
            case["legacy"] = True
    if corpus.name in overrides:
        if not corpus.migratable:
            raise SystemExit(f"{corpus.name}: sources cannot be overridden")
        apply_overrides(cases, overrides[corpus.name])
    ids = [case["id"] for case in cases]
    duplicates = [cid for cid, n in collections.Counter(ids).items() if n > 1]
    if duplicates:
        raise SystemExit(f"{corpus.name}: duplicate case ids: {duplicates[:5]}")
    selected = args.cases.get(corpus.name) if args.cases is not None else None
    if selected is not None:
        unknown = set(selected) - set(ids)
        if unknown:
            raise SystemExit(f"{corpus.name}: unknown case ids: {sorted(unknown)[:5]}")
        cases = [case for case in cases if case["id"] in selected]
        ids = [case["id"] for case in cases]
    by_id = {case["id"]: case for case in cases}
    runs = 2 if args.record else 1
    observed = []
    for _ in range(runs):
        if corpus.kind == "engine":
            observed.append(run_engine(args.harness, cases, args.jobs, corpus.name))
        elif corpus.kind == "cli":
            observed.append(run_cli(args.bin, cases, args.jobs))
        else:
            observed.append(run_lsp(args.bin, cases, args.jobs))
    golden = {}
    if not corpus.legacy and corpus.golden.exists():
        golden = {record["id"]: record for record in read_jsonl(corpus.golden)}
    table = None
    if corpus.kind == "lsp":
        replies = GOLDEN / "lsp.replies.jsonl.gz"
        table = Replies(read_jsonl(replies) if replies.exists() else ())
    actual, counters, varies = {}, {}, {}
    for cid in ids:
        records, counts = [], []
        for observations in observed:
            observation = observations[cid]
            if corpus.kind == "engine":
                record, count = engine_record(observation)
                if count is not None:
                    counts.append(count)
            elif corpus.kind == "cli":
                record = cli_record(observation)
            else:
                record = lsp_record(observation, table)
            records.append(record)
        actual[cid] = records[0]
        if any(r != records[0] for r in records[1:]):
            varies[cid] = outcome_class(records[0])
        if counts:
            counters[cid] = counts[0] if all(count == counts[0] for count in counts) else []
    # When recording, differences from the previous goldens are the changes being accepted.
    compared = not args.record
    problems = collections.OrderedDict((name, {"blocking": blocking, "cases": []}) for name, blocking in [
        ("crashes", True), ("expectation failures", True),
        ("observable differences", compared), ("unrecorded cases", compared), ("stale goldens", compared),
        ("position drift in migrated sources", False),
        ("quota outcomes that followed accounting drift", compared and args.strict_quota),
    ])
    add = lambda category, cid, message: problems[category]["cases"].append((cid, message))  # noqa: E731
    for cid in ids:
        observation = observed[0][cid]
        if observation.get("phase") in ("panic", "crash", "hang") or "failure" in observation:
            add("crashes", cid, describe(actual[cid]))
        if corpus.kind == "engine":
            message = expectation_failure(by_id[cid], observation)
            if message:
                # A legacy corpus keeps its goldens as these expectations.
                add("observable differences" if corpus.legacy else "expectation failures", cid, message)
    if corpus.legacy:
        problems["observable differences"]["blocking"] = True
    if not corpus.legacy and (golden or not args.record):
        for cid in ids:
            # A static rejection is checked above; its golden keeps the outcome
            # it had in the ADR-004 language.
            if "_static_error" in by_id[cid]:
                continue
            compare_case(by_id[cid], golden.get(cid), actual[cid], varies.get(cid), table, add)
        for cid in sorted(set(golden) - set(ids)) if selected is None else ():
            add("stale goldens", cid, "golden has no case")
    notes = []
    if corpus.kind == "engine":
        varying = {cid for cid, record in golden.items() if "varies" in record} | set(varies)
        counter_lines, drifted = counter_notes(
            load_counters(corpus), {k: v for k, v in counters.items() if k not in varying}, args.show)
        notes += counter_lines
        if drifted and args.strict_counters and not args.record:
            add("observable differences", corpus.name, "counters drifted (--strict-counters)")
    if args.observations and corpus.kind == "engine":
        with open(args.observations, "a", encoding="utf-8") as out:
            for cid in ids:
                out.write(canonical({"corpus": corpus.name, **observed[0][cid]}) + "\n")
    elapsed = f"{time.monotonic() - started:.0f}s"
    if not args.record:
        report.section(corpus.name, len(ids), problems, notes + [f"checked in {elapsed}"])
    elif any(entry["blocking"] and entry["cases"] for entry in problems.values()):
        report.section(corpus.name, len(ids), problems, notes + ["not recorded"])
    else:
        rejected = {cid for cid in ids if "_static_error" in by_id[cid]}
        record_corpus(corpus, ids, actual, counters, varies, table, preserve=selected is not None,
                      keep=rejected)
        report.section(corpus.name, len(ids), problems, notes + [
            f"recorded in {elapsed}" + (f"; {len(varies)} cases vary between runs" if varies else "")])


def compare_case(case, expected, got, varies, table, add):
    """Classifies how one observation differs from its golden, if it does."""
    cid = case["id"]
    if expected is None:
        add("unrecorded cases", cid, describe(got))
        return
    expected = {k: v for k, v in expected.items() if k != "id"}
    if "varies" in expected:
        if outcome_class(got) != expected["varies"]:
            add("observable differences", cid, f"expected a varying {expected['varies']} outcome, got {describe(got)}")
        return
    if varies is not None:
        got = {"varies": varies}
    if expected == got:
        return
    category = "observable differences"
    quota = case.get("_quota") and any(r.get("error", {}).get("kind") in QUOTA_KINDS for r in (expected, got))
    unplaced = [{**r, "error": {k: v for k, v in r["error"].items() if k != "at"}} if "error" in r else r
                for r in (expected, got)]
    if case.get("_overridden") and unplaced[0] == unplaced[1]:
        category = "position drift in migrated sources"
    elif quota:
        category = "quota outcomes that followed accounting drift"
    if table is not None and "replies" in expected and "replies" in got:
        message = lsp_difference(cid, expected["replies"], got["replies"], table)
    else:
        message = difference(expected, got)
    add(category, cid, message)


def lsp_difference(document, expected, got, table):
    transcripts = load_script("lsp-transcripts")
    if document == "protocol":
        labels = [f"protocol message {i}" for i in range(len(transcripts.PROTOCOL))]
    else:
        uri = "file:///corpus/" + document
        session = transcripts.session(uri, read_source(ROOT / document))
        labels = ["initialize", "initialized"] + [
            f"{m['method']} {canonical(m['params'].get('position'))}" for m, _ in session] + ["shutdown"]
    for index, (a, b) in enumerate(zip(expected, got)):
        if a != b:
            label = labels[index] if index < len(labels) else f"message {index}"
            return (f"{label}: expected {table.bodies[a][:300]}\n      got      {table.bodies[b][:300]}")
    return f"expected {len(expected)} replies, got {len(got)}"


def record_corpus(corpus, ids, actual, counters, varies, table, preserve=False, keep=frozenset()):
    """Records selected observations, preserving other cases when requested.

    The cases in `keep`, static rejections, keep their recorded goldens and
    counters, the outcomes they had in the ADR-004 language; one without a
    golden records its compile error."""
    previous = {record["id"]: record for record in read_jsonl(corpus.golden)} \
        if not corpus.legacy and corpus.golden.exists() else {}
    if not corpus.legacy:
        records = dict(previous) if preserve else {}
        for cid in keep:
            if cid in previous:
                records[cid] = previous[cid]
        for cid in sorted(ids):
            if cid in keep and cid in previous:
                continue
            if cid in varies:
                records[cid] = {"id": cid, "varies": varies[cid]}
            else:
                records[cid] = {"id": cid, **actual[cid]}
        records = [records[cid] for cid in sorted(records)]
        if table is not None:
            compact_replies(records, table)
        write_jsonl(corpus.golden, records)
    if counters or preserve and corpus.counters.exists():
        recorded = load_counters(corpus)
        kept = {cid: values for cid, values in recorded.items() if cid not in ids} if preserve else {}
        kept.update({cid: values for cid, values in counters.items() if cid not in varies and cid not in keep})
        kept.update({cid: recorded[cid] for cid in keep if cid in recorded})
        write_jsonl(corpus.counters, [[cid, *kept[cid]] for cid in sorted(kept)])
    if table is not None:
        write_jsonl(GOLDEN / "lsp.replies.jsonl.gz", [{"reply": body} for body in table.bodies])


def compact_replies(records, table):
    """Renumbers reply indexes by first use and drops replies no session gives."""
    order = {}
    for record in records:
        for index in record.get("replies", []):
            order.setdefault(index, len(order))
    for record in records:
        if "replies" in record:
            record["replies"] = [order[index] for index in record["replies"]]
    table.bodies = [table.bodies[index] for index in sorted(order, key=order.get)]


def build(args, corpora):
    kinds = {corpus.kind for corpus in corpora}
    if "engine" in kinds and args.harness == HARNESS:
        subprocess.run([str(CARGO), "build", "--profile", "gate", "--locked", "--example", "golden"], cwd=ROOT, check=True)
    if kinds & {"cli", "lsp"} and args.bin == VIBES:
        subprocess.run([str(CARGO), "build", "--profile", "gate", "--locked", "-p", "vibes"], cwd=ROOT, check=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus", help="comma-separated corpora to run (default: all); see --list")
    parser.add_argument("--list", action="store_true", help="list the corpora and exit")
    parser.add_argument("--harness", type=Path, default=HARNESS, help="the examples/golden binary")
    parser.add_argument("--bin", type=Path, default=VIBES, help="the vibes binary, for the cli and lsp corpora")
    parser.add_argument("--no-build", action="store_true", help="use the given binaries without building them")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--show", type=int, default=10, help="examples shown per category")
    parser.add_argument("--record", action="store_true",
                        help="record the goldens from this build instead of checking them")
    parser.add_argument("--cases", type=Path, metavar="FILE",
                        help='run only case ids from a JSON map {"corpus": ["id", ...]}; recording preserves other cases')
    parser.add_argument("--observations", type=Path, metavar="FILE",
                        help="also write every engine case's raw observation, one JSON line each")
    parser.add_argument("--strict-counters", action="store_true", help="fail when accounting counters drift")
    parser.add_argument("--strict-quota", action="store_true",
                        help="fail when a quota-limited case changes outcome under a quota error")
    parser.add_argument("--export", type=Path, metavar="DIR",
                        help="write every case source to DIR/<corpus>/, with an index.json, and exit")
    parser.add_argument("--sources", type=Path, metavar="DIR",
                        help="run the sources in an --export tree (such as a migrated copy) against the goldens")
    parser.add_argument("--override", type=Path, metavar="FILE",
                        help='a JSON map {"corpus": {"source key": "source"}} of sources to run instead')
    parser.add_argument("--failures", type=Path, metavar="FILE",
                        help="also write every problem's case ids, by corpus and category, as JSON")
    args = parser.parse_args(argv)
    if args.list:
        for corpus in CORPORA.values():
            print(f"{corpus.name:14} {corpus.description}")
        return 0
    if args.cases is not None:
        args.cases = json.loads(args.cases.read_text())
        if not isinstance(args.cases, dict) or any(
                not isinstance(ids, list) or any(not isinstance(cid, str) for cid in ids)
                for ids in args.cases.values()):
            parser.error("--cases expects a JSON object mapping corpus names to lists of case ids")
        args.cases = {name: set(ids) for name, ids in args.cases.items()}
    names = args.corpus.split(",") if args.corpus else list(args.cases if args.cases is not None else CORPORA)
    if args.cases is not None and any(name not in args.cases for name in names):
        parser.error("--cases must include every selected corpus")
    unknown = [name for name in names if name not in CORPORA]
    if unknown:
        parser.error(f"unknown corpus {', '.join(unknown)}; choose from {', '.join(CORPORA)}")
    corpora = [CORPORA[name] for name in names]
    if not args.no_build and not args.export:
        build(args, corpora)
    args.harness, args.bin = args.harness.resolve(), args.bin.resolve()
    if args.export:
        export(corpora, args.export.resolve())
        return 0
    overrides = {}
    if args.sources:
        overrides = load_sources(args.sources)
    if args.override:
        for name, sources in json.loads(args.override.read_text()).items():
            overrides.setdefault(name, {}).update(sources)
    if args.record and overrides:
        parser.error("--record records the original sources; drop --sources and --override")
    report = Report(args.show)
    if args.observations:
        args.observations.write_text("")
    for corpus in corpora:
        check_corpus(corpus, args, overrides, report)
    if args.failures:
        args.failures.write_text(canonical(report.cases) + "\n")
    return 1 if report.failed else 0


if __name__ == "__main__":
    sys.exit(main())
