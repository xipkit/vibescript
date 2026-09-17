package main

import (
	"encoding/json"
	"os"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes"
)

func TestCheckerBoundaryReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-boundaries.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Template string
		Cases    []struct {
			Source   string
			Target   string
			Rejected bool
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) == 0 {
		t.Fatal("checker boundary corpus is empty")
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		source := strings.NewReplacer("SOURCE", fixture.Source, "TARGET", fixture.Target).Replace(fixtures.Template)
		script, err := engine.Compile(source)
		if err != nil {
			t.Fatalf("%s -> %s: %v", fixture.Source, fixture.Target, err)
		}
		warnings := script.CheckWarnings()
		if rejected := len(warnings) != 0; rejected != fixture.Rejected {
			t.Errorf("%s -> %s rejected=%t, want %t: %v", fixture.Source, fixture.Target, rejected, fixture.Rejected, warnings)
		}
		for _, warning := range warnings {
			if !strings.HasPrefix(warning.Message, "return value expected") {
				t.Errorf("%s -> %s produced unrelated diagnostic: %v", fixture.Source, fixture.Target, warning)
			}
		}
	}
}

func TestCheckerFlowReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-flow.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 60 {
		t.Fatalf("checker flow corpus has %d cases, want 60", len(fixtures.Cases))
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			t.Fatalf("Compile(%q): %v", fixture.Source, err)
		}
		warnings := script.CheckWarnings()
		if rejected := len(warnings) != 0; rejected != fixture.GoRejected {
			t.Errorf("CheckWarnings(%q) rejected=%t, want %t: %v", fixture.Source, rejected, fixture.GoRejected, warnings)
		}
	}
}

func TestCheckerCallReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-calls.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 52 {
		t.Fatalf("checker call corpus has %d cases, want 52", len(fixtures.Cases))
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			t.Fatalf("Compile(%q): %v", fixture.Source, err)
		}
		warnings := script.CheckWarningsForFunction("run")
		if rejected := len(warnings) != 0; rejected != fixture.GoRejected {
			t.Errorf("CheckWarningsForFunction(run) in %q rejected=%t, want %t: %v", fixture.Source, rejected, fixture.GoRejected, warnings)
		}
	}
}

func TestCheckerCollectionReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-collections.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 92 {
		t.Fatalf("checker collection corpus has %d cases, want 92", len(fixtures.Cases))
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			t.Fatalf("Compile(%q): %v", fixture.Source, err)
		}
		warnings := script.CheckWarningsForFunction("run")
		if rejected := len(warnings) != 0; rejected != fixture.GoRejected {
			t.Errorf("CheckWarningsForFunction(run) in %q rejected=%t, want %t: %v", fixture.Source, rejected, fixture.GoRejected, warnings)
		}
	}
}

func TestCheckerAddressReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-addresses.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 66 {
		t.Fatalf("checker address corpus has %d cases, want 66", len(fixtures.Cases))
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			t.Fatalf("Compile(%q): %v", fixture.Source, err)
		}
		warnings := script.CheckWarningsForFunction("run")
		if rejected := len(warnings) != 0; rejected != fixture.GoRejected {
			t.Errorf("CheckWarningsForFunction(run) in %q rejected=%t, want %t: %v", fixture.Source, rejected, fixture.GoRejected, warnings)
		}
	}
}

func TestCheckerRecursionReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-recursion.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 21 {
		t.Fatalf("checker recursion corpus has %d cases, want 21", len(fixtures.Cases))
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		script, err := engine.Compile(fixture.Source)
		if err != nil {
			t.Fatalf("Compile(%q): %v", fixture.Source, err)
		}
		warnings := script.CheckWarningsForFunction("run")
		if rejected := len(warnings) != 0; rejected != fixture.GoRejected {
			t.Errorf("CheckWarningsForFunction(run) in %q rejected=%t, want %t: %v", fixture.Source, rejected, fixture.GoRejected, warnings)
		}
	}
}
