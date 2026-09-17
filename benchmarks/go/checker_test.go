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

func TestCheckerIterationReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-iteration.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Cases []struct {
			Source     string
			GoRejected bool `json:"go_rejected"`
		}
		SyntaxDifferences []struct {
			Source         string
			GoCompileError string `json:"go_compile_error"`
		} `json:"syntax_differences"`
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) != 49 || len(fixtures.SyntaxDifferences) != 1 {
		t.Fatalf("checker iteration corpus has %d decisions and %d syntax differences, want 49 and 1", len(fixtures.Cases), len(fixtures.SyntaxDifferences))
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
	for _, fixture := range fixtures.SyntaxDifferences {
		_, err := engine.Compile(fixture.Source)
		if err == nil || err.Error() != fixture.GoCompileError {
			t.Errorf("Compile(%q) error=%v, want %s", fixture.Source, err, fixture.GoCompileError)
		}
	}
}

func TestCheckerCaseReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-case.json")
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
	if len(fixtures.Cases) != 53 {
		t.Fatalf("checker case corpus has %d cases, want 53", len(fixtures.Cases))
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

func TestCheckerExceptionsReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-exceptions.json")
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
	if len(fixtures.Cases) != 43 {
		t.Fatalf("checker exception corpus has %d cases, want 43", len(fixtures.Cases))
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

func TestCheckerBuiltinsReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-builtins.json")
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
	if len(fixtures.Cases) != 67 {
		t.Fatalf("checker builtin corpus has %d cases, want 67", len(fixtures.Cases))
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

func TestCheckerNativeReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-native.json")
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
	if len(fixtures.Cases) != 90 {
		t.Fatalf("checker native corpus has %d cases, want 90", len(fixtures.Cases))
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

func TestCheckerValuesReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-values.json")
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
	if len(fixtures.Cases) != 113 {
		t.Fatalf("checker value corpus has %d cases, want 113", len(fixtures.Cases))
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

func TestCheckerProtectedReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-protected.json")
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
	if len(fixtures.Cases) != 108 {
		t.Fatalf("checker protected corpus has %d cases, want 108", len(fixtures.Cases))
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

func TestCheckerBlocksReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-blocks.json")
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
	if len(fixtures.Cases) != 12 {
		t.Fatalf("checker block corpus has %d cases, want 12", len(fixtures.Cases))
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

func TestCheckerYieldReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-yield.json")
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
	if len(fixtures.Cases) != 22 {
		t.Fatalf("checker yield corpus has %d cases, want 22", len(fixtures.Cases))
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

func TestCheckerLexicalReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-lexical.json")
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
	if len(fixtures.Cases) != 16 {
		t.Fatalf("checker lexical corpus has %d cases, want 16", len(fixtures.Cases))
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

func TestCheckerForwardingReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-forwarding.json")
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
	if len(fixtures.Cases) != 18 {
		t.Fatalf("checker forwarding corpus has %d cases, want 18", len(fixtures.Cases))
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

func TestCheckerCollectionBlocksReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-collection-blocks.json")
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
	if len(fixtures.Cases) != 30 {
		t.Fatalf("checker collection block corpus has %d cases, want 30", len(fixtures.Cases))
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

func TestCheckerReductionsReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-reductions.json")
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
	if len(fixtures.Cases) != 43 {
		t.Fatalf("checker reduction corpus has %d cases, want 43", len(fixtures.Cases))
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

func TestCheckerGroupingReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-grouping.json")
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
	if len(fixtures.Cases) != 49 {
		t.Fatalf("checker grouping corpus has %d cases, want 49", len(fixtures.Cases))
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

func TestCheckerSchedulesReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-schedules.json")
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
	if len(fixtures.Cases) != 50 {
		t.Fatalf("checker schedule corpus has %d cases, want 50", len(fixtures.Cases))
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

func TestCheckerSelectionsReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-selections.json")
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
	if len(fixtures.Cases) != 63 {
		t.Fatalf("checker selection corpus has %d cases, want 63", len(fixtures.Cases))
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

func TestCheckerOrderingReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-ordering.json")
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
	if len(fixtures.Cases) != 68 {
		t.Fatalf("checker ordering corpus has %d cases, want 68", len(fixtures.Cases))
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
