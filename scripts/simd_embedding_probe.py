"""Reuse public-call workloads in an ordinary embedding executable."""

from pathlib import Path
import hashlib
import json
import re
import sys


def prepare(source, destination):
    fixtures = ["simd_controls", "ascii_case", "ascii_case_compare", "json_spans", "regexp_scan", "whitespace"]
    destination.mkdir(parents=True)
    hashes = {}
    benchmarks = []
    for fixture in fixtures:
        original = source / "internal/runtime" / (fixture + "_benchmark_test.go")
        text = original.read_text().replace("package runtime\n", "package main\n", 1)
        if fixture == "ascii_case":
            text = text.split("var asciiCaseBenchmarkSink string")[0]
        if fixture == "json_spans":
            old = "builtinJSONParse(nil, NewNil(), []Value{NewString(raw.String())}, nil, NewNil())"
            assert old in text
            text = text.replace(old, "parseProbeJSON(raw.String())")
        target = destination / (fixture + ".go")
        target.write_text(text.rstrip() + "\n")
        hashes[original.name] = {"source": hashlib.sha256(original.read_bytes()).hexdigest(), "generated": hashlib.sha256(target.read_bytes()).hexdigest()}
        benchmarks.extend(re.findall(r"func (Benchmark\w+)\(b \*testing.B\)", text))
    main = '''package main

import (
    "context"
    "regexp"
    "testing"

    "github.com/mgomes/vibescript/vibes"
    "github.com/mgomes/vibescript/vibes/value"
)

type Config = vibes.Config
type Engine = vibes.Engine
type Script = vibes.Script
type CallOptions = vibes.CallOptions
type Value = value.Value

func MustNewEngine(config Config) *Engine { return vibes.MustNewEngine(config) }
func NewString(text string) Value { return value.NewString(text) }
func NewInt(n int64) Value { return value.NewInt(n) }
func NewNil() Value { return value.NewNil() }
func NewHash(entries map[string]Value) Value { return value.NewHash(entries) }

func parseProbeJSON(text string) (Value, error) {
    engine := vibes.MustNewEngine(vibes.Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20})
    script, err := engine.Compile("def run(s) JSON.parse(s) end")
    if err != nil { return value.NewNil(), err }
    return script.Call(context.Background(), "run", []value.Value{value.NewString(text)}, vibes.CallOptions{})
}

func main() {
    testing.Main(regexp.MatchString, nil, []testing.InternalBenchmark{
'''
    main += "".join(f'        {{Name: "{name}", F: {name}}},\n' for name in benchmarks)
    main += "    }, nil)\n}\n"
    (destination / "main.go").write_text(main)
    (destination / "fixture-hashes.json").write_text(json.dumps(hashes, indent=2) + "\n")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]), Path(sys.argv[2]))
