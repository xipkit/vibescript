package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func uniqAccountingRows(count int) Value {
	rows := make([]Value, count)
	for i := range rows {
		rows[i] = NewHash(map[string]Value{"id": NewInt(int64(i))})
	}
	return NewArray(rows)
}

func TestArrayUniqRegionsPreserveQuotaThresholds(t *testing.T) {
	payload := strings.Repeat("p", 2000)
	peak := strings.Repeat("z", 3000)
	sources := map[string]string{
		"scalar":     "def run()\n [1, 2, 3, 4, 1, 2].uniq\nend",
		"composite":  "def run()\n [[1], [2], [3], [4], [1]].uniq\nend",
		"scalar_key": "def run()\n [[1], [2], [3], [4]].uniq do |row| row[0] end\nend",
		"retained_key": fmt.Sprintf(`def run()
  [1, 2, 3, 4].uniq do |n|
    n.to_s + %q
  end
end`, payload),
		"mutate_receiver": fmt.Sprintf(`def run()
  [[1], [2], [3], [4]].uniq do |row|
    row.push(%q)
    %q
    row[0]
  end
end`, payload, peak),
		"rebind_outer": fmt.Sprintf(`def run()
  buf = "seed"
  [1, 2, 3, 4].uniq do |n|
    buf = buf + %q
    %q
    n
  end
end`, payload, peak),
		"nested_driver": fmt.Sprintf(`def run()
  [[1], [2], [3], [4]].uniq do |row|
    row.map do |n|
      %q
      n
    end
  end
end`, peak),
	}
	// The large live ballast keeps the equality scratch below its pricing
	// granule. String comparisons still cross periodic memory-check boundaries
	// while the hash key slices are live, so those checks cannot be skipped.
	fields := make([]string, 0, 16)
	for i := range 16 {
		fields = append(fields, fmt.Sprintf("k%d: payload", i))
	}
	hash := "{" + strings.Join(fields, ", ") + "}"
	sources["equality_scratch"] = fmt.Sprintf("def run()\n ballast = %q\n payload = %q\n [%s, %s].uniq\nend",
		strings.Repeat("b", 512<<10), strings.Repeat("p", 4096), hash, hash)

	for name, source := range sources {
		t.Run(name, func(t *testing.T) {
			memoized := memoMinimalPassingQuota(t, source)
			baseWalkCacheDisabled.Store(true)
			t.Cleanup(func() { baseWalkCacheDisabled.Store(false) })
			unmemoized := memoMinimalPassingQuota(t, source)
			baseWalkCacheDisabled.Store(false)
			if memoized != unmemoized {
				t.Errorf("uniq quota threshold = %d bytes with regions, want %d bytes from full graph walks", memoized, unmemoized)
			}
		})
	}
}

func TestArrayUniqMemoryWalkScalesWithReceiver(t *testing.T) {
	if estimatorVerify {
		t.Skip("the estimator oracle intentionally repeats full graph walks")
	}
	for _, expression := range []string{
		"values.uniq",
		"values.uniq do |row| row[:id] end",
	} {
		t.Run(expression, func(t *testing.T) {
			script := compileScriptWithConfig(t, Config{
				StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20,
			}, "def run(values)\n"+expression+"\nend")
			visits := func(count int) uint64 {
				args := []Value{uniqAccountingRows(count)}
				estimatorVisits.Store(0)
				estimatorVisitCounting.Store(true)
				defer estimatorVisitCounting.Store(false)
				got, err := script.Call(context.Background(), "run", args, CallOptions{})
				if err != nil {
					t.Fatalf("run(%d rows) error = %v", count, err)
				}
				if len(got.Array()) != count {
					t.Fatalf("run(%d rows) returned %d rows, want %d", count, len(got.Array()), count)
				}
				return estimatorVisits.Load()
			}
			small, large := visits(100), visits(200)
			if small == 0 {
				t.Fatal("run(100 rows) visited no estimator nodes, want an exercised memory walk")
			}
			// Membership comparisons can be quadratic for composites; walking
			// the reachable receiver for quota checks must scale with its size.
			if large > small*3 {
				t.Errorf("doubling uniq rows grew memory walks from %d to %d nodes, want at most 3x", small, large)
			}
		})
	}
}

func TestArrayUniqRestoresRegionOnExit(t *testing.T) {
	for _, tc := range []struct {
		name       string
		expression string
		wantError  bool
	}{
		{name: "blockless", expression: "checked_uniq(values)"},
		{name: "block", expression: "checked_uniq(values) do |row| row[:id] end"},
		{name: "return", expression: "checked_uniq(values) do |row| return row end"},
		{name: "raise", expression: `checked_uniq(values) do |row| raise "stop" end`, wantError: true},
		{name: "nested", expression: "[values].map do |rows| checked_uniq(rows) do |row| row[:id] end end"},
		{name: "cancel", expression: "checked_uniq(values) do |row| cancel_now() end", wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20})
			calls := 0
			engine.RegisterBuiltin("checked_uniq", func(exec *Execution, _ Value, args []Value, _ map[string]Value, block Value) (Value, error) {
				active, boundary, depth := exec.blockRegionActive, exec.blockRegionBoundary, exec.blockRegionBuiltinDepth
				scratch := exec.reservedScratchBytes
				got, err := arrayUniq(exec, args[0], nil, nil, block, "array.uniq")
				calls++
				if exec.blockRegionActive != active || exec.blockRegionBoundary != boundary || exec.blockRegionBuiltinDepth != depth {
					t.Errorf("uniq left region (%t, %d, %d), want enclosing region (%t, %d, %d)",
						exec.blockRegionActive, exec.blockRegionBoundary, exec.blockRegionBuiltinDepth, active, boundary, depth)
				}
				if exec.reservedScratchBytes != scratch {
					t.Errorf("uniq left %d scratch bytes, want %d bytes before call", exec.reservedScratchBytes, scratch)
				}
				return got, err
			})
			engine.RegisterBuiltin("cancel_now", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
				cancel()
				return NewNil(), exec.checkContext()
			})
			script := compileScriptWithEngine(t, engine, "def run(values)\n"+tc.expression+"\nend")
			_, err := script.Call(ctx, "run", []Value{uniqAccountingRows(4)}, CallOptions{})
			if (err != nil) != tc.wantError {
				t.Errorf("run(4 rows) error = %v, want error presence %t", err, tc.wantError)
			}
			if calls != 1 {
				t.Errorf("run(4 rows) completed %d uniq calls, want 1", calls)
			}
		})
	}
}

func TestArrayUniqCountsEqualityScratchDuringProbe(t *testing.T) {
	const fields = 16
	payload := NewString(strings.Repeat("p", 4096))
	rows := make([]Value, 2)
	for i := range rows {
		entries := make(map[string]Value, fields)
		for key := range fields {
			entries[fmt.Sprintf("k%d", key)] = payload
		}
		rows[i] = NewHash(entries)
	}
	receiver := NewArray(rows)
	exec := &Execution{ctx: context.Background(), quota: Unlimited, memoryQuota: 64 << 20, root: newEnv(nil)}
	exec.root.Define("values", receiver)
	exec.root.Define("ballast", NewString(strings.Repeat("b", 512<<10)))
	base := exec.estimateMemoryUsageForCallRoots(NewNil(), receiver, nil, nil, NewNil())
	// The operation holds its roots slice, two result slots, and one distinct
	// composite slot. Leave space for those, but not both hash key sort slices.
	held := valueSliceScratchBytes(1) + valueSliceScratchBytes(2) + valueSliceScratchBytes(1)
	exec.memoryQuota = base + held + 256
	if scratch := 2 * fields * hashKeySortScratchEntryBytes; scratch >= exec.equalityScratchGranule() {
		t.Fatalf("probe scratch = %d bytes, want less than the %d-byte dedicated-check granule", scratch, exec.equalityScratchGranule())
	}
	_, err := arrayUniq(exec, receiver, nil, nil, NewNil(), "array.uniq")
	requireErrorIs(t, err, errMemoryQuotaExceeded)
	if exec.equalityScratchReserved != 0 || exec.reservedScratchBytes != 0 {
		t.Errorf("uniq left equality scratch %d and loop scratch %d bytes, want both released", exec.equalityScratchReserved, exec.reservedScratchBytes)
	}
}

func BenchmarkArrayUniqAccounting(b *testing.B) {
	for _, quota := range []int{Unlimited, 64 << 20} {
		for _, count := range []int{200, 400, 800} {
			for _, expression := range []string{
				"values.uniq",
				"values.uniq do |row| row end",
				"values.uniq do |row| row[:id] end",
			} {
				b.Run(fmt.Sprintf("quota=%d/n=%d/%s", quota, count, expression), func(b *testing.B) {
					script := compileScriptWithConfig(b, Config{
						StepQuota: Unlimited, MemoryQuotaBytes: quota,
					}, "def run(values)\n"+expression+"\nend")
					args := []Value{uniqAccountingRows(count)}
					b.ReportAllocs()
					b.ResetTimer()
					for range b.N {
						got, err := script.Call(context.Background(), "run", args, CallOptions{})
						if err != nil {
							b.Fatalf("run(%d rows) error = %v", count, err)
						}
						if len(got.Array()) != count {
							b.Fatalf("run(%d rows) returned %d rows, want %d", count, len(got.Array()), count)
						}
					}
				})
			}
		}
	}
}
