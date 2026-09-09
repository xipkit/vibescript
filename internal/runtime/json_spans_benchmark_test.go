package runtime

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
)

func jsonSpanBenchmarkPayload(kind string, n int) string {
	pattern := "abcdefghijklmnopqrstuvwxyz012345"
	switch kind {
	case "sparse-escape":
		pattern = strings.Repeat("a", min(63, n-1)) + "\n"
	case "dense-escape":
		pattern = "a\n\t\"\\"
	case "unicode":
		pattern = "é界🙂"
		if n < len(pattern) {
			pattern = "é"
		}
		return strings.Repeat(pattern, n/len(pattern)) + strings.Repeat("a", n%len(pattern))
	case "mixed-unicode":
		prefix := min(64, n/2)
		return strings.Repeat("a", prefix) + jsonSpanBenchmarkPayload("unicode", n-prefix)
	case "html":
		pattern = "<tag>&value</tag>"
	case "invalid-utf8":
		pattern = "abc\xff\xfe"
	}
	return strings.Repeat(pattern, (n+len(pattern)-1)/len(pattern))[:n]
}

func BenchmarkJSONSpans(b *testing.B) {
	for _, n := range []int{16, 4096, 65536} {
		for _, kind := range []string{"ascii", "sparse-escape", "dense-escape", "unicode", "mixed-unicode", "html", "invalid-utf8"} {
			text := jsonSpanBenchmarkPayload(kind, n)
			encoded, err := json.Marshal(text)
			if err != nil {
				b.Fatal(err)
			}
			if kind == "unicode" || kind == "mixed-unicode" || kind == "invalid-utf8" || kind == "html" {
				encoded = []byte("\"" + text + "\"")
			}
			raw := `{"payload":` + string(encoded) + `,"id":7}`
			payload := NewHash(map[string]Value{"payload": NewString(text), "id": NewInt(7)})
			for _, operation := range []string{"parse", "stringify"} {
				b.Run(fmt.Sprintf("%s/%d/%s", operation, n, kind), func(b *testing.B) {
					source := "def run(input)\n  JSON.parse(input)\nend"
					input := NewString(raw)
					if operation == "stringify" {
						source = "def run(input)\n  JSON.stringify(input)\nend"
						input = payload
					}
					script := simdBenchmarkCompileWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, source)
					args := []Value{input}
					b.ReportAllocs()
					b.ResetTimer()
					for range b.N {
						if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
							b.Fatal(err)
						}
					}
				})
			}
		}
	}
}

func BenchmarkJSONSpansMixedDocument(b *testing.B) {
	var raw strings.Builder
	raw.WriteByte('[')
	for i := range 48 {
		if i != 0 {
			raw.WriteByte(',')
		}
		text := jsonSpanBenchmarkPayload("ascii", 24+i*37)
		switch i % 4 {
		case 1:
			text = "é界🙂" + text
		case 2:
			text = text + "\n<end>&"
		case 3:
			text = strings.Repeat("a", 64) + jsonSpanBenchmarkPayload("unicode", 384)
		}
		encoded, err := json.Marshal(text)
		if err != nil {
			b.Fatal(err)
		}
		fmt.Fprintf(&raw, `{"id":%d,"active":true,"text":%s,"tags":["ok","短い",null]}`, i, encoded)
	}
	raw.WriteByte(']')
	payload, err := builtinJSONParse(nil, NewNil(), []Value{NewString(raw.String())}, nil, NewNil())
	if err != nil {
		b.Fatal(err)
	}
	for _, operation := range []string{"parse", "stringify"} {
		b.Run(operation, func(b *testing.B) {
			source := "def run(input)\n  JSON.parse(input)\nend"
			input := NewString(raw.String())
			if operation == "stringify" {
				source = "def run(input)\n  JSON.stringify(input)\nend"
				input = payload
			}
			script := simdBenchmarkCompileWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, source)
			args := []Value{input}
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
