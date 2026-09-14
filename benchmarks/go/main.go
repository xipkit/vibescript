package main

import (
	"context"
	"encoding/json"
	"fmt"
	"hash/fnv"
	"os"
	"runtime"
	"strconv"
	"time"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type fixture struct {
	Function    string            `json:"function"`
	Name        string            `json:"name"`
	Source      string            `json:"source"`
	Args        []json.RawMessage `json:"args"`
	Accounting  bool              `json:"accounting"`
	Iterations  int               `json:"iterations"`
	EntropyByte *byte             `json:"entropy_byte,omitempty"`
}

type repeatingByte byte

// Read fills p with the fixture's fixed entropy byte.
func (b repeatingByte) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = byte(b)
	}
	return len(p), nil
}

var sink value.Value

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 4 {
		return fmt.Errorf("usage: compare FIXTURES ITERATIONS MODE")
	}
	raw, err := os.ReadFile(os.Args[1])
	if err != nil {
		return err
	}
	var fixtures []fixture
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		return err
	}
	n, err := strconv.Atoi(os.Args[2])
	if err != nil || n < 0 {
		return fmt.Errorf("invalid iteration count")
	}
	mode := os.Args[3]
	if mode != "validate" && mode != "timing" && mode != "alloc" {
		return fmt.Errorf("invalid mode")
	}
	codec, err := vibes.NewEngine(vibes.Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20})
	if err != nil {
		return err
	}
	converter, err := codec.Compile("def parse(s)\n JSON.parse(s)\nend\ndef encode(v)\n JSON.stringify(v)\nend")
	if err != nil {
		return err
	}
	ctx := context.Background()
	enc := json.NewEncoder(os.Stdout)
	for _, fixture := range fixtures {
		function := fixture.Function
		if function == "" {
			function = "run"
		}
		cfg := vibes.Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20, RecursionLimit: 256}
		if !fixture.Accounting {
			cfg.StepQuota = vibes.Unlimited
			cfg.MemoryQuotaBytes = vibes.Unlimited
		}
		if fixture.EntropyByte != nil {
			cfg.RandomReader = repeatingByte(*fixture.EntropyByte)
		}
		engine, err := vibes.NewEngine(cfg)
		if err != nil {
			return err
		}
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			return fmt.Errorf("%s: %w", fixture.Name, err)
		}
		args := make([]value.Value, len(fixture.Args))
		for i, raw := range fixture.Args {
			args[i], err = converter.Call(ctx, "parse", []value.Value{value.NewString(string(raw))}, vibes.CallOptions{})
			if err != nil {
				return fmt.Errorf("%s argument: %w", fixture.Name, err)
			}
		}
		result, err := script.Call(ctx, function, args, vibes.CallOptions{})
		if err != nil {
			return fmt.Errorf("%s: %w", fixture.Name, err)
		}
		encoded, err := converter.Call(ctx, "encode", []value.Value{result}, vibes.CallOptions{})
		if err != nil {
			return err
		}
		output := encoded.String()
		digest := fnv.New64a()
		if _, err := digest.Write([]byte(output)); err != nil {
			return err
		}
		record := map[string]any{"name": fixture.Name, "digest": fmt.Sprintf("%016x", digest.Sum64()), "output_bytes": len(output)}
		if mode == "validate" {
			record["result_json"] = output
		} else {
			iterations := n
			if iterations == 0 {
				iterations = fixture.Iterations
			}
			if iterations <= 0 {
				return fmt.Errorf("missing iterations for %s", fixture.Name)
			}
			for range min(32, iterations) {
				sink, err = script.Call(ctx, function, args, vibes.CallOptions{})
				if err != nil {
					return err
				}
			}
			sink = value.NewNil()
			runtime.GC()
			var before, after runtime.MemStats
			if mode == "alloc" {
				runtime.ReadMemStats(&before)
			}
			start := time.Now()
			for range iterations {
				sink, err = script.Call(ctx, function, args, vibes.CallOptions{})
				if err != nil {
					return err
				}
			}
			elapsed := time.Since(start)
			if mode == "alloc" {
				runtime.ReadMemStats(&after)
				record["alloc_bytes"] = float64(after.TotalAlloc-before.TotalAlloc) / float64(iterations)
				record["allocations"] = float64(after.Mallocs-before.Mallocs) / float64(iterations)
			}
			finalOutput, err := converter.Call(ctx, "encode", []value.Value{sink}, vibes.CallOptions{})
			if err != nil {
				return err
			}
			if finalOutput.String() != output {
				return fmt.Errorf("%s: timed output differs from validation", fixture.Name)
			}
			record["iterations"] = iterations
			record["ns_per_call"] = float64(elapsed.Nanoseconds()) / float64(iterations)
		}
		if err := enc.Encode(record); err != nil {
			return err
		}
	}
	return nil
}
