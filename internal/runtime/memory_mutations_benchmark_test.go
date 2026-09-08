package runtime

import (
	"fmt"
	"testing"
)

func BenchmarkMemoryMemoMutationIsolation(b *testing.B) {
	for _, rows := range []int{1000, 10000} {
		for _, traffic := range []string{"none", "environment", "wrapper", "overflow"} {
			b.Run(fmt.Sprintf("rows=%d/%s", rows, traffic), func(b *testing.B) {
				exec, env := newEstimatorCacheExec()
				env.Define("rows", estimatorCacheRows(rows))
				unrelated := newEnv(nil)
				first, second := NewString("first"), NewString("second")
				unrelated.Define("state", first)
				hash := NewHash(map[string]Value{"state": first})
				key := NewString("state")
				want := exec.estimateMemoryUsage()
				walked := exec.memoryEst.walked
				b.ReportAllocs()
				b.ResetTimer()
				for i := range b.N {
					next := first
					if i%2 == 0 {
						next = second
					}
					switch traffic {
					case "environment":
						unrelated.Define("state", next)
					case "wrapper":
						if err := hash.HashSet(key, next); err != nil {
							b.Fatal(err)
						}
					case "overflow":
						for range 1000 {
							if err := hash.HashSet(key, next); err != nil {
								b.Fatal(err)
							}
						}
					}
					if got := exec.estimateMemoryUsage(); got != want {
						b.Fatalf("estimate = %d, want %d", got, want)
					}
				}
				b.ReportMetric(float64(exec.memoryEst.walked-walked)/float64(b.N), "nodes/op")
			})
		}
	}
}
