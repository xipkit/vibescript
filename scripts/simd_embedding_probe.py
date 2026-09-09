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
    "os"
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
    if path := os.Getenv("SIMD_PROFILE_FILE"); path != "" {
        if err := profileWorkload(path, os.Getenv("SIMD_PROFILE_WORKLOAD")); err != nil { panic(err) }
        return
    }
    testing.Main(regexp.MatchString, nil, []testing.InternalBenchmark{
'''
    main += "".join(f'        {{Name: "{name}", F: {name}}},\n' for name in benchmarks)
    main += "    }, nil)\n}\n"
    (destination / "main.go").write_text(main)
    (destination / "profile.go").write_text('''package main

import (
    "context"
    "encoding/json"
    "fmt"
    "os"
    "runtime/pprof"
    "time"
)

func profileWorkload(path, workload string) error {
    var source string
    var args []Value
    switch workload {
    case "json":
        encoded, err := json.Marshal(jsonSpanBenchmarkPayload("dense-escape", 65536))
        if err != nil { return err }
        source = "def run(input) JSON.parse(input) end"
        args = []Value{NewString(`{"payload":` + string(encoded) + `,"id":7}`)}
    case "index":
        source = "def run(text, needle, n) total = 0; for i in 1..n; total = total + text.index(needle); end; total; end"
        args = []Value{NewString(simdBenchmarkUnicodeStringText()), NewString("終"), NewInt(200)}
    case "rindex":
        source = "def run(text, needle, n) total = 0; for i in 1..n; total = total + text.rindex(needle); end; total; end"
        args = []Value{NewString(simdBenchmarkUnicodeStringText()), NewString("é"), NewInt(200)}
    default:
        return fmt.Errorf("unknown profile workload %q", workload)
    }
    script, err := MustNewEngine(Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}).Compile(source)
    if err != nil { return err }
    if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil { return err }
    file, err := os.Create(path)
    if err != nil { return err }
    defer file.Close()
    if err := pprof.StartCPUProfile(file); err != nil { return err }
    defer pprof.StopCPUProfile()
    until := time.Now().Add(3 * time.Second)
    calls := 0
    for time.Now().Before(until) {
        if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil { return err }
        calls++
    }
    fmt.Printf("Profiled %s: %d calls\\n", workload, calls)
    return nil
}
''')
    (destination / "fixture-hashes.json").write_text(json.dumps(hashes, indent=2) + "\n")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]), Path(sys.argv[2]))
