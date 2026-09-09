package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"
)

func BenchmarkFormatLiteralSpans(b *testing.B) {
	for _, size := range []int{16, 4096, 65536} {
		literal := strings.Repeat("abcdefgh", size/8)
		for _, tc := range []struct {
			name    string
			pattern string
			call    string
		}{
			{name: "ASCII", pattern: literal, call: "format(pattern)"},
			{name: "Unicode", pattern: strings.Repeat("éΣ", size/4), call: "format(pattern)"},
			{name: "Sparse", pattern: literal + "%s", call: `format(pattern, "value")`},
		} {
			b.Run(fmt.Sprintf("%d/%s", size, tc.name), func(b *testing.B) {
				benchmarkFormatLiteralCall(b, tc.call, []Value{NewString(tc.pattern)}, false)
			})
		}
	}
	for _, tc := range []struct {
		name    string
		pattern string
		call    string
		wantErr bool
	}{
		{name: "ShortNumber", pattern: "value=%08.2f", call: "format(pattern, 12.5)"},
		{name: "Dense", pattern: strings.Repeat("%[1]d:", 512), call: "format(pattern, 42)"},
		{name: "Escaped", pattern: strings.Repeat("%%a", 1024), call: "format(pattern)"},
		{name: "EscapedTwo", pattern: strings.Repeat("%%ab", 1024), call: "format(pattern)"},
		{name: "EscapedFour", pattern: strings.Repeat("%%abcd", 1024), call: "format(pattern)"},
		{name: "InvalidUTF8", pattern: strings.Repeat("a\xffb", 1365), call: "format(pattern)"},
		{name: "Malformed", pattern: strings.Repeat("x", 4096) + "%*s", call: `format(pattern, "value")`, wantErr: true},
	} {
		b.Run(tc.name, func(b *testing.B) {
			benchmarkFormatLiteralCall(b, tc.call, []Value{NewString(tc.pattern)}, tc.wantErr)
		})
	}
}

func BenchmarkStrftimeLiteralSpans(b *testing.B) {
	tm := NewTime(time.Date(2024, 1, 2, 3, 4, 5, 123456789, time.UTC))
	for _, size := range []int{16, 4096, 65536} {
		literal := strings.Repeat("abcdefgh", size/8)
		for _, tc := range []struct {
			name    string
			pattern string
		}{
			{name: "ASCII", pattern: literal},
			{name: "Unicode", pattern: strings.Repeat("éΣ", size/4)},
			{name: "Sparse", pattern: literal + "%Y"},
		} {
			b.Run(fmt.Sprintf("%d/%s", size, tc.name), func(b *testing.B) {
				benchmarkFormatLiteralCall(b, "t.strftime(pattern)", []Value{NewString(tc.pattern), tm}, false)
			})
		}
	}
	for _, tc := range []struct {
		name    string
		pattern string
		wantErr bool
	}{
		{name: "ShortDate", pattern: "%Y-%m-%d %H:%M:%S"},
		{name: "Dense", pattern: strings.Repeat("%d:", 1024)},
		{name: "Escaped", pattern: strings.Repeat("%%a", 1024)},
		{name: "EscapedTwo", pattern: strings.Repeat("%%ab", 1024)},
		{name: "InvalidUTF8", pattern: strings.Repeat("a\xffb", 1365)},
		{name: "Malformed", pattern: strings.Repeat("x", 4096) + "%", wantErr: true},
	} {
		b.Run(tc.name, func(b *testing.B) {
			benchmarkFormatLiteralCall(b, "t.strftime(pattern)", []Value{NewString(tc.pattern), tm}, tc.wantErr)
		})
	}
}

func benchmarkFormatLiteralCall(b *testing.B, call string, args []Value, wantErr bool) {
	b.Helper()
	params := "pattern"
	if len(args) == 2 {
		params += ", t"
	}
	script := simdBenchmarkCompileWithConfig(b, Config{
		StepQuota:        5_000_000,
		MemoryQuotaBytes: 64 << 20,
	}, "def run("+params+") "+call+" end")
	b.ReportAllocs()
	b.SetBytes(int64(len(args[0].String())))
	b.ResetTimer()
	for range b.N {
		if _, err := script.Call(context.Background(), "run", args, CallOptions{}); (err != nil) != wantErr {
			b.Fatalf("%s error = %v, want error presence %t", call, err, wantErr)
		}
	}
}
