package runtime

import (
	"context"
	"fmt"
	goruntime "runtime"
	"runtime/debug"
	"strconv"
	"strings"
	"testing"
	"unicode/utf8"
)

type literalCompiler struct {
	name       string
	entrypoint string
	snippet    bool
	compile    func(string) (*Script, error)
}

func literalCompilers(engine *Engine) []literalCompiler {
	const snippetEntry = "__literal_benchmark__"
	return []literalCompiler{
		{name: "Compile", entrypoint: "run", compile: engine.Compile},
		{
			name:       "CompileSnippet",
			entrypoint: snippetEntry,
			snippet:    true,
			compile: func(source string) (*Script, error) {
				return engine.CompileSnippet(source, snippetEntry)
			},
		},
	}
}

func (compiler literalCompiler) source(body string) string {
	if compiler.snippet {
		return body + "\n"
	}
	return "def run\n" + body + "\nend\n"
}

func BenchmarkCompileLiteralSpans(b *testing.B) {
	for _, compiler := range literalCompilers(benchmarkEngine()) {
		for _, quote := range []string{`"`, `'`} {
			quoteName := "double"
			if quote == `'` {
				quoteName = "single"
			}
			for _, size := range []int{16, 4096, 65536} {
				sparseUnit := strings.Repeat("a", min(size, 64)-2) + `\\`
				for _, fixture := range []struct {
					name    string
					body    string
					wantErr bool
				}{
					{name: "ascii", body: strings.Repeat("a", size)},
					{name: "sparse_escapes", body: repeatLiteralUnit(sparseUnit, size)},
					{name: "dense_escapes", body: repeatLiteralUnit(`\\`, size)},
					{name: "unicode", body: repeatLiteralUnit("éé🙂", size)},
					{name: "early_error", body: "a\x00" + strings.Repeat("a", size-2), wantErr: true},
				} {
					name := fmt.Sprintf("%s/%s/%s/bytes_%d", compiler.name, quoteName, fixture.name, size)
					b.Run(name, func(b *testing.B) {
						if len(fixture.body) != size || !utf8.ValidString(fixture.body) {
							b.Fatalf("%s fixture has %d bytes and valid UTF-8 = %t, want %d and true",
								fixture.name, len(fixture.body), utf8.ValidString(fixture.body), size)
						}
						source := compiler.source(quote + fixture.body + quote)
						benchmarkLiteralCompilation(b, compiler, source, fixture.wantErr)
					})
				}
			}
		}
	}
}

// Repeat complete UTF-8 and escape units, then fill any remainder with ASCII.
// Truncating a repeated byte string could benchmark an invalid final literal.
func repeatLiteralUnit(unit string, size int) string {
	return strings.Repeat(unit, size/len(unit)) + strings.Repeat("a", size%len(unit))
}

func BenchmarkCompileRepeatedSmallLiterals(b *testing.B) {
	for _, compiler := range literalCompilers(benchmarkEngine()) {
		for _, fixture := range []struct {
			name  string
			quote string
		}{
			{name: "double", quote: `"`},
			{name: "single", quote: `'`},
		} {
			b.Run(compiler.name+"/"+fixture.name, func(b *testing.B) {
				body := strings.Repeat(fixture.quote+"abcdefghijklmnop"+fixture.quote+"\n", 1000)
				benchmarkLiteralCompilation(b, compiler, compiler.source(body), false)
			})
		}
	}
}

func BenchmarkCompileSnippetRepresentativeWorkloads(b *testing.B) {
	compiler := literalCompilers(benchmarkEngine())[1]
	for _, fixture := range []struct {
		name string
		path string
	}{
		{name: "control_flow", path: "tests/complex/loops.vibe"},
		{name: "typed", path: "tests/complex/typed.vibe"},
		{name: "massive", path: "tests/complex/massive.vibe"},
	} {
		b.Run(fixture.name, func(b *testing.B) {
			source := benchmarkSourceFromFile(b, fixture.path) + "\nrun()\n"
			benchmarkLiteralCompilation(b, compiler, source, false)
		})
	}
}

func benchmarkLiteralCompilation(b *testing.B, compiler literalCompiler, source string, wantErr bool) {
	b.Helper()

	if script, err := compiler.compile(source); (err != nil) != wantErr || (err == nil && script == nil) {
		b.Fatalf("%s(%d source bytes) returned script = %t, error = %v, want error presence = %t",
			compiler.name, len(source), script != nil, err, wantErr)
	}
	b.ReportAllocs()
	b.SetBytes(int64(len(source)))
	for b.Loop() {
		script, err := compiler.compile(source)
		if (err != nil) != wantErr || (err == nil && script == nil) {
			b.Fatalf("%s(%d source bytes) returned script = %t, error = %v, want error presence = %t",
				compiler.name, len(source), script != nil, err, wantErr)
		}
	}
}

func TestCompileLiteralSpanHeapEvidence(t *testing.T) {
	// These samples read process-wide heap and temporarily disable GC, so they
	// must run serially. The input and engine exist before the baseline sample.
	const bodyBytes = 64 << 10
	for _, compiler := range literalCompilers(benchmarkEngine()) {
		for _, quote := range []string{`"`, `'`} {
			t.Run(compiler.name+"/"+strconv.Quote(quote), func(t *testing.T) {
				body := strings.Repeat("a", bodyBytes)
				source := compiler.source(quote + body + quote)
				compileLiteralValue(t, compiler, compiler.source(quote+"warm"+quote))
				collectLiteralCompileGarbage()
				var before, peak, retained goruntime.MemStats
				var script *Script
				var err error
				func() {
					gcPercent := debug.SetGCPercent(-1)
					defer debug.SetGCPercent(gcPercent)
					goruntime.ReadMemStats(&before)
					script, err = compiler.compile(source)
					goruntime.ReadMemStats(&peak)
				}()
				if err != nil || script == nil {
					t.Fatalf("%s(%d source bytes) returned script = %t, error = %v, want a compiled script",
						compiler.name, len(source), script != nil, err)
				}
				if peak.NumGC != before.NumGC {
					t.Fatalf("%s heap sample collected %d times with GC disabled, want no collections",
						compiler.name, peak.NumGC-before.NumGC)
				}
				collectLiteralCompileGarbage()
				goruntime.ReadMemStats(&retained)
				peakBytes := int64(peak.HeapAlloc) - int64(before.HeapAlloc)
				retainedBytes := int64(retained.HeapAlloc) - int64(before.HeapAlloc)
				t.Logf("%s: %d-byte ASCII body; peak heap growth with GC disabled = %d bytes; retained with script alive after two GCs = %d bytes",
					compiler.name, bodyBytes, peakBytes, retainedBytes)
				got := callScript(t, context.Background(), script, compiler.entrypoint, nil, CallOptions{})
				if got.Kind() != KindString || got.String() != body {
					t.Errorf("%s literal result has kind %v and %d bytes, want string with %d identical ASCII bytes",
						compiler.name, got.Kind(), len(got.String()), bodyBytes)
				}
				goruntime.KeepAlive(script)
				goruntime.KeepAlive(source)
				goruntime.KeepAlive(body)
			})
		}
	}
}

func TestCompiledPlainLiteralsReleaseSourceDocuments(t *testing.T) {
	// Keeping just a literal must not keep the Script's whole source alive.
	// The 16 distinct source allocations total 8 MiB; allow 2 MiB for noise.
	const documents = 16
	const documentBytes = 512 << 10
	for _, compiler := range literalCompilers(benchmarkEngine()) {
		for _, quote := range []string{`"`, `'`} {
			t.Run(compiler.name+"/"+strconv.Quote(quote), func(t *testing.T) {
				compileLiteralValue(t, compiler, compiler.source(quote+"warm"+quote))
				collectLiteralCompileGarbage()
				var before, after goruntime.MemStats
				goruntime.ReadMemStats(&before)
				kept := make([]Value, documents)
				for i := range kept {
					kept[i] = compileTinyLiteralDocument(t, compiler, quote, i, documentBytes)
					if got := kept[i]; got.Kind() != KindString || got.String() != "x" {
						t.Fatalf("%s document %d returned %s, want string %q", compiler.name, i, got.Inspect(), "x")
					}
				}
				collectLiteralCompileGarbage()
				goruntime.ReadMemStats(&after)
				held := int64(after.HeapAlloc) - int64(before.HeapAlloc)
				t.Logf("%s: %d one-byte literals from %d-byte documents retain %d heap bytes",
					compiler.name, documents, documentBytes, held)
				if held > 2<<20 {
					t.Errorf("%s retains %d heap bytes for %d one-byte literals, want <=2 MiB",
						compiler.name, held, documents)
				}
				goruntime.KeepAlive(kept)
			})
		}
	}
}

func compileTinyLiteralDocument(t testing.TB, compiler literalCompiler, quote string, id, size int) Value {
	t.Helper()

	prefix := "# document " + strconv.Itoa(id) + "\n#"
	suffix := "\n" + compiler.source(quote+"x"+quote)
	source := prefix + strings.Repeat("a", size-len(prefix)-len(suffix)) + suffix
	return compileLiteralValue(t, compiler, source)
}

func compileLiteralValue(t testing.TB, compiler literalCompiler, source string) Value {
	t.Helper()

	script, err := compiler.compile(source)
	if err != nil || script == nil {
		t.Fatalf("%s(%d source bytes) returned script = %t, error = %v, want a compiled script",
			compiler.name, len(source), script != nil, err)
	}
	return callScript(t, context.Background(), script, compiler.entrypoint, nil, CallOptions{})
}

// Two collections clear both generations of temporary sync.Pool storage.
func collectLiteralCompileGarbage() {
	goruntime.GC()
	goruntime.GC()
}
