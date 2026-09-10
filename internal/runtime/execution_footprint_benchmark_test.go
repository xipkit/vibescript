package runtime

import (
	"context"
	"fmt"
	"testing"
)

func BenchmarkExecutionCallDepth(b *testing.B) {
	for _, depth := range []int64{0, 2, 3, 6, 7, 30, 126} {
		b.Run(fmt.Sprintf("depth=%d", depth), func(b *testing.B) {
			script := compileScriptWithEngine(b, benchmarkEngine(), `def descend(depth)
  if depth == 0
    1
  else
    descend(depth - 1)
  end
end
def run(depth)
  descend(depth)
end`)
			args := []Value{NewInt(depth)}
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				got, err := script.Call(context.Background(), "run", args, CallOptions{})
				if err != nil || got.Int() != 1 {
					b.Fatalf("Call(depth=%d) = %v, %v, want 1, nil", depth, got, err)
				}
			}
		})
	}
}
