package reference

import (
	"encoding/hex"
	"encoding/json"
	"math/rand"
	"os"
	"path"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type matchCase struct {
	Pattern string `json:"pattern_hex"`
	Name    string `json:"name_hex"`
	Matched bool   `json:"matched"`
	Invalid bool   `json:"invalid"`
}

func TestGlobReference(t *testing.T) {
	dir := fixtureDir(t)
	var cases []matchCase
	add := func(pattern, name string) {
		matched, err := path.Match(pattern, name)
		cases = append(cases, matchCase{
			Pattern: hex.EncodeToString([]byte(pattern)),
			Name:    hex.EncodeToString([]byte(name)),
			Matched: matched,
			Invalid: err != nil,
		})
	}
	patterns := []string{
		"", "*", "**", "?", "??", "[ab]", "[^a]", "[/]", "[^/]", "[z-a]",
		"[a-z]", "[é-ê]", "[[]", "[\\]]", "\\*", "a*b*c", "*a?b*", "*[/]*",
		"*[^a]*b", "*\xa9", "[", "[^]", "[-a]", "[a-]", "[a-b-c]", "[\xff]",
		"pkg/*", "pkg/?", "pkg/**/name", ".*", "*.vibe", "*.vibe.vibe", "\\",
	}
	names := []string{
		"", "a", "b", "ab", "axbyc", "aaaaab", "/", "a/b", "pkg/tool", "pkg/x/name",
		".vibe", "tool.vibe.vibe", " tool ", "é", "ê", "aé", "é/b", "\xff", "\xa9", "a\xffb",
	}
	for _, pattern := range patterns {
		for _, name := range names {
			add(pattern, name)
		}
	}
	random := rand.New(rand.NewSource(17001))
	parts := []string{"a", "b", "/", "*", "?", "[ab]", "[^a]", "[/]", "[é-ê]", "é", "\xff", "[", "\\", "]", "-"}
	letters := []string{"a", "b", "/", "é", "ê", "\xff", "\xa9"}
	for range 8192 {
		var pattern, name strings.Builder
		for range random.Intn(7) {
			pattern.WriteString(parts[random.Intn(len(parts))])
		}
		for range random.Intn(9) {
			name.WriteString(letters[random.Intn(len(letters))])
		}
		add(pattern.String(), name.String())
	}
	data, err := json.Marshal(cases)
	if err != nil {
		t.Fatalf("encode glob reference: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "glob-reference.json"), append(data, '\n'), 0600); err != nil {
		t.Fatalf("write glob reference: %v", err)
	}
	t.Logf("recorded %d path.Match observations", len(cases))
}

type policyCase struct {
	Allow   []string `json:"allow"`
	Deny    []string `json:"deny"`
	Name    string   `json:"name_hex"`
	Allowed bool     `json:"allowed"`
	Invalid bool     `json:"invalid"`
}

func TestPolicyReference(t *testing.T) {
	dir := fixtureDir(t)
	root := tempDir(t)
	writeModule(t, root, "runner", "def run(name);require(name).answer;end")
	// The runner supplies a module origin; the receiving engine applies policy.
	bootstrap := probeEngine(t, vibes.Config{ModulePaths: []string{root}})
	maker := compile(t, bootstrap, "def make;require(\"runner\");end")
	runner, err := maker.Call(t.Context(), "make", nil, vibes.CallOptions{})
	if err != nil {
		t.Fatalf("create module caller for relative requests: %v", err)
	}
	for _, name := range []string{
		"tool.vibe", "tool.vibe.vibe", ".vibe", ".vibe.vibe", "pkg/tool.vibe", "pkg/deeper/tool.vibe",
		"pkg/..vibe", "pkg/...vibe", " tool .vibe", "tool .vibe", "pkg/ tool.vibe", "pkg.vibe",
		".vibe ", "é.vibe", "pkg/.vibe", "pkg/tool.vibe.vibe",
	} {
		file := filepath.Join(root, name)
		if err := os.MkdirAll(filepath.Dir(file), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(file, []byte("def answer;42;end"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	names := []string{
		"tool", " tool ", "tool.vibe", "tool.vibe.vibe", ".vibe", ".vibe.vibe",
		"pkg/tool", "pkg\\tool", "pkg/deeper/tool", "pkg/..vibe", "pkg/...vibe", "pkg",
		"./ tool /.", "./tool /.", "pkg/ tool", "./.vibe /.", "é", "pkg/.vibe", "pkg/tool.vibe.vibe",
	}
	patterns := []string{
		"*", "*.vibe", "**", "tool", "tool.vibe", "tool.vibe.vibe", ".vibe", ".vibe.vibe",
		"pkg/*", "pkg/*.vibe", "pkg/*/*.vibe", "pkg\\tool", "pkg/..vibe", "pkg/...vibe",
		"pkg", "pkg.vibe", "./ tool /.", "./tool /.", "./.vibe /.", "pkg/ tool",
		"é", "[a-z]*", "[é-ê]", "pkg/[.]*", "t???", "pkg/tool.vibe.vibe",
		"", ".", " ", " tool ", "[", "[a-]", "[^]", "[a-b-c]",
	}
	var cases []policyCase
	for _, pattern := range patterns {
		for _, denyMode := range []bool{false, true} {
			allow := []string{pattern}
			deny := []string{}
			if denyMode {
				allow = []string{"*"}
				deny = []string{pattern}
			}
			engine, err := vibes.NewEngine(vibes.Config{
				ModulePaths: []string{root}, ModuleAllowList: allow, ModuleDenyList: deny,
				StepQuota: 100000, MemoryQuotaBytes: 8 << 20,
			})
			if err != nil {
				cases = append(cases, policyCase{Allow: allow, Deny: deny, Invalid: true})
				continue
			}
			script := compile(t, engine, "def run(ns,name);ns.run(name);end")
			for _, name := range names {
				v, err := script.Call(t.Context(), "run", []value.Value{runner, value.NewString(name)}, vibes.CallOptions{})
				row := policyCase{Allow: allow, Deny: deny, Name: hex.EncodeToString([]byte(name)), Allowed: err == nil}
				if err == nil {
					if v.Kind() != value.KindInt || v.Int() != 42 {
						t.Fatalf("policy %q name %q result: got %v, want int 42", pattern, name, v)
					}
				} else if !strings.Contains(err.Error(), "denied by policy") && !strings.Contains(err.Error(), "not allowed by policy") {
					t.Fatalf("policy %q (deny=%v) name %q did not reach policy result or fixture: %v", pattern, denyMode, name, err)
				}
				cases = append(cases, row)
			}
		}
	}
	data, err := json.Marshal(cases)
	if err != nil {
		t.Fatalf("encode policy reference: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "policy-reference.json"), append(data, '\n'), 0600); err != nil {
		t.Fatalf("write policy reference: %v", err)
	}
	t.Logf("recorded %d module policy observations", len(cases))
}

func fixtureDir(t *testing.T) string {
	t.Helper()
	if runtime.Version() != "go1.27.1" {
		t.Fatalf("reference toolchain: got %s, want go1.27.1", runtime.Version())
	}
	dir := os.Getenv("VIBE_POLICY_FIXTURES")
	if dir == "" {
		t.Fatal("VIBE_POLICY_FIXTURES must name the fixture output directory")
	}
	return dir
}

func tempDir(t *testing.T) string {
	t.Helper()
	repo, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	cache := filepath.Join(repo, ".cache", "tmp")
	if err := os.MkdirAll(cache, 0700); err != nil {
		t.Fatal(err)
	}
	root, err := os.MkdirTemp(cache, "module-policy-reference-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := os.RemoveAll(root); err != nil {
			t.Errorf("remove module fixtures: %v", err)
		}
	})
	return root
}

func probeEngine(t *testing.T, cfg vibes.Config) *vibes.Engine {
	t.Helper()
	cfg.StepQuota = 100000
	cfg.MemoryQuotaBytes = 8 << 20
	cfg.RecursionLimit = 128
	engine, err := vibes.NewEngine(cfg)
	if err != nil {
		t.Fatalf("create reference engine: %v", err)
	}
	return engine
}

func compile(t *testing.T, engine *vibes.Engine, source string) *vibes.Script {
	t.Helper()
	script, err := engine.Compile(source)
	if err != nil {
		t.Fatalf("compile source %q: %v", source, err)
	}
	return script
}

func writeModule(t *testing.T, root, name, source string) {
	t.Helper()
	file := filepath.Join(root, name+".vibe")
	if err := os.MkdirAll(filepath.Dir(file), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(file, []byte(source), 0600); err != nil {
		t.Fatal(err)
	}
}
