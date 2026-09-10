package runtime

import (
	"context"
	"fmt"
	"testing"
)

func BenchmarkJSONStringifyObjectShapes(b *testing.B) {
	for _, tc := range []struct {
		name   string
		width  int
		depth  int
		object bool
	}{
		{name: "small_hash", width: 4, depth: 1},
		{name: "large_hash", width: 32, depth: 1},
		{name: "small_object", width: 4, depth: 1, object: true},
		{name: "large_object", width: 32, depth: 1, object: true},
		{name: "nested_hash", width: 4, depth: 64},
	} {
		b.Run(tc.name, func(b *testing.B) {
			input := memorySnapshotBenchmarkValue(tc.width, tc.depth)
			if tc.object {
				input = NewObject(input.HashEntryMap())
			}
			script := compileScriptWithEngine(b, benchmarkEngine(), "def run(input)\nJSON.stringify(input)\nend")
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

func BenchmarkHostCloneNestedHashes(b *testing.B) {
	for _, depth := range []int{4, 64, 256} {
		b.Run(fmt.Sprintf("depth=%d", depth), func(b *testing.B) {
			input := memorySnapshotBenchmarkValue(4, depth)
			var got Value
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				got = cloneValueForHost(input)
			}
			b.StopTimer()
			if !got.Equal(input) {
				b.Fatal("host clone changed the nested value")
			}
		})
	}
}

func memorySnapshotBenchmarkValue(width, depth int) Value {
	input := NewString("leaf")
	for range depth {
		hash := NewHashWithCapacity(width)
		setClonedHashEntry(hash, NewString("child"), input)
		for i := range width - 1 {
			setClonedHashEntry(hash, NewString(fmt.Sprintf("key%d", i)), NewInt(int64(i)))
		}
		input = hash
	}
	return input
}
