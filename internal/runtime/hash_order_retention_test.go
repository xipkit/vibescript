package runtime

import "testing"

func TestDeletedHashKeysReleasePayloads(t *testing.T) {
	const source = `def run(bytes, count)
  kept = []
  measure_heap()
  for i in 1..count
    key = "z" * bytes
    row = {}
    row["keep"] = 1
    row[key] = 2
    row.delete(key)
    kept.push(row)
  end
  key = nil
  row = nil
  measure_heap()
  19
end`
	held := returnedFrameHeapBytes(t, source, []Value{NewInt(1 << 20), NewInt(24)})
	t.Logf("24 deleted one-MiB keys retain %d bytes", held)
	if limit := int64(2 << 20); held > limit {
		t.Errorf("deleted hash keys retain %d bytes, want less than %d", held, limit)
	}
}
