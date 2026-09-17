package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"hash/fnv"
	"io"
	"os"
	"runtime"
	"strconv"
	"time"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type fixture struct {
	SignatureProbe    *signatureProbe            `json:"signature_probe,omitempty"`
	BlockProbe        bool                       `json:"block_probe,omitempty"`
	Function          string                     `json:"function"`
	Name              string                     `json:"name"`
	Source            string                     `json:"source"`
	Args              []json.RawMessage          `json:"args"`
	Globals           map[string]json.RawMessage `json:"globals,omitempty"`
	StrictEffects     bool                       `json:"strict_effects,omitempty"`
	CapabilityProbe   bool                       `json:"capability_probe,omitempty"`
	Notifications     []string                   `json:"notifications,omitempty"`
	ResultEncoding    string                     `json:"result_encoding,omitempty"`
	AllowRequire      bool                       `json:"allow_require,omitempty"`
	ModulePaths       []string                   `json:"module_paths,omitempty"`
	ModuleAllow       []string                   `json:"module_allow,omitempty"`
	ModuleDeny        []string                   `json:"module_deny,omitempty"`
	ModuleDevelopment bool                       `json:"module_development,omitempty"`
	Accounting        bool                       `json:"accounting"`
	Iterations        int                        `json:"iterations"`
	EntropyByte       *byte                      `json:"entropy_byte,omitempty"`
	Stdout            bool                       `json:"stdout,omitempty"`
	Stderr            bool                       `json:"stderr,omitempty"`
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
		cfg := vibes.Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20, RecursionLimit: 256, StrictEffects: fixture.StrictEffects}
		cfg.ModulePaths = fixture.ModulePaths
		cfg.ModuleAllowList = fixture.ModuleAllow
		cfg.ModuleDenyList = fixture.ModuleDeny
		cfg.DevMode = fixture.ModuleDevelopment
		if !fixture.Accounting {
			cfg.StepQuota = vibes.Unlimited
			cfg.MemoryQuotaBytes = vibes.Unlimited
		}
		if fixture.EntropyByte != nil {
			cfg.RandomReader = repeatingByte(*fixture.EntropyByte)
		}
		var stdout, stderr bytes.Buffer
		if fixture.Stdout {
			cfg.OutputWriter = io.Discard
			if mode == "validate" {
				cfg.OutputWriter = &stdout
			}
		}
		if fixture.Stderr {
			cfg.ErrorWriter = io.Discard
			if mode == "validate" {
				cfg.ErrorWriter = &stderr
			}
		}
		engine, err := vibes.NewEngine(cfg)
		if err != nil {
			return err
		}
		if probe := fixture.SignatureProbe; probe != nil && probe.Registration == "registered" {
			if err := engine.RegisterBuiltinWithSignature("echo", probe.call, probe.signature()); err != nil {
				return err
			}
		}
		var script *vibes.Script
		if function == "__main__" {
			script, err = engine.CompileSnippet(fixture.Source, function)
		} else {
			script, err = engine.Compile(fixture.Source)
		}
		if err != nil {
			return fmt.Errorf("%s: compile error: %w", fixture.Name, err)
		}
		args := make([]value.Value, len(fixture.Args))
		for i, raw := range fixture.Args {
			args[i], err = converter.Call(ctx, "parse", []value.Value{value.NewString(string(raw))}, vibes.CallOptions{})
			if err != nil {
				return fmt.Errorf("%s argument: %w", fixture.Name, err)
			}
		}
		options := vibes.CallOptions{AllowRequire: fixture.AllowRequire}
		if fixture.CapabilityProbe {
			options.Capabilities = []vibes.CapabilityAdapter{probeCapability{}}
		}
		if fixture.BlockProbe {
			options.Capabilities = append(options.Capabilities, blockCapability{})
		}
		for _, name := range fixture.Notifications {
			options.Capabilities = append(options.Capabilities, notificationCapability{name: name})
		}
		if len(fixture.Globals) > 0 {
			options.Globals = make(map[string]value.Value, len(fixture.Globals))
			for name, raw := range fixture.Globals {
				global, err := converter.Call(ctx, "parse", []value.Value{value.NewString(string(raw))}, vibes.CallOptions{})
				if err != nil {
					return fmt.Errorf("%s global %q: %w", fixture.Name, name, err)
				}
				options.Globals[name] = global
			}
		}
		if probe := fixture.SignatureProbe; probe != nil {
			switch probe.Registration {
			case "registered":
			case "global":
				method, err := probe.method()
				if err != nil {
					return err
				}
				if options.Globals == nil {
					options.Globals = make(map[string]value.Value)
				}
				options.Globals["echo"] = method
			default:
				options.Capabilities = append(options.Capabilities, *probe)
			}
		}
		result, err := script.Call(ctx, function, args, options)
		if err != nil {
			return fmt.Errorf("%s: %w", fixture.Name, err)
		}
		output, err := encodeResult(ctx, converter, result, fixture.ResultEncoding)
		if err != nil {
			return err
		}
		digest := fnv.New64a()
		if _, err := digest.Write([]byte(output)); err != nil {
			return err
		}
		record := map[string]any{"name": fixture.Name, "digest": fmt.Sprintf("%016x", digest.Sum64()), "output_bytes": len(output)}
		if mode == "validate" {
			record["result_json"] = output
			if fixture.Stdout {
				record["stdout_hex"] = hex.EncodeToString(stdout.Bytes())
			}
			if fixture.Stderr {
				record["stderr_hex"] = hex.EncodeToString(stderr.Bytes())
			}
		} else {
			iterations := n
			if iterations == 0 {
				iterations = fixture.Iterations
			}
			if iterations <= 0 {
				return fmt.Errorf("missing iterations for %s", fixture.Name)
			}
			for range min(32, iterations) {
				sink, err = script.Call(ctx, function, args, options)
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
				sink, err = script.Call(ctx, function, args, options)
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
			finalOutput, err := encodeResult(ctx, converter, sink, fixture.ResultEncoding)
			if err != nil {
				return err
			}
			if finalOutput != output {
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

type probeCapability struct{}

// Bind creates independent capability state for one comparison invocation.
func (probeCapability) Bind(_ vibes.CapabilityBinding) (map[string]value.Value, error) {
	count := int64(0)
	next := vibes.NewBuiltin("host.next", func(_ *vibes.Execution, _ value.Value, _ []value.Value, _ map[string]value.Value, _ value.Value) (value.Value, error) {
		count++
		return value.NewInt(count), nil
	})
	checked := vibes.NewBuiltin("host.checked", func(_ *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, _ value.Value) (value.Value, error) {
		count++
		if args[0].Int() == 0 {
			return value.NewString("invalid result"), nil
		}
		return args[0], nil
	})
	factory := vibes.NewBuiltin("host.factory", func(_ *vibes.Execution, _ value.Value, _ []value.Value, _ map[string]value.Value, _ value.Value) (value.Value, error) {
		return value.NewObject(map[string]value.Value{"checked": checked}), nil
	})
	echo := vibes.NewBuiltin("host.echo", func(_ *vibes.Execution, _ value.Value, args []value.Value, kwargs map[string]value.Value, _ value.Value) (value.Value, error) {
		return value.NewArray([]value.Value{value.NewArray(args), value.NewHash(kwargs)}), nil
	})
	fail := vibes.NewBuiltin("host.fail", func(_ *vibes.Execution, _ value.Value, _ []value.Value, _ map[string]value.Value, _ value.Value) (value.Value, error) {
		return value.NewNil(), fmt.Errorf("host failure")
	})
	return map[string]value.Value{"host": value.NewObject(map[string]value.Value{
		"next": next, "checked": checked, "factory": factory, "echo": echo, "map": echo, "fail": fail,
		"items": value.NewArray([]value.Value{value.NewInt(1)}),
	})}, nil
}

// CapabilityContracts checks both sides of the comparison's typed host boundary.
func (probeCapability) CapabilityContracts() map[string]vibes.CapabilityMethodContract {
	return map[string]vibes.CapabilityMethodContract{
		"host.checked": {
			ValidateArgs: func(args []value.Value, kwargs map[string]value.Value, _ value.Value) error {
				if len(args) != 1 || args[0].Kind() != value.KindInt || len(kwargs) != 0 {
					return fmt.Errorf("host.checked expects one integer")
				}
				return nil
			},
			ValidateReturn: func(result value.Value) error {
				if result.Kind() != value.KindInt {
					return fmt.Errorf("host.checked must return an integer")
				}
				return nil
			},
		},
	}
}
