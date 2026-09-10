package runtime

import (
	"fmt"
	"testing"
)

func TestReturnedReceiversReleasePayloads(t *testing.T) {
	const source = `class RetentionBox
  def initialize(bytes)
    @payload = "x" * bytes
  end
  def touch(depth, fail)
    if depth > 0
      touch(depth - 1, fail)
    elsif fail
      raise("stop")
    end
    7
  end
end
def run(bytes, depth, fail)
  measure_heap()
  box = RetentionBox.new(bytes)
  begin
    box.touch(depth, fail)
  rescue
    nil
  end
  box = nil
  measure_heap()
  19
end`
	for _, depth := range []int64{1, 16} {
		for _, fail := range []bool{false, true} {
			t.Run(fmt.Sprintf("depth=%d/fail=%t", depth, fail), func(t *testing.T) {
				held := returnedFrameHeapBytes(t, source, []Value{NewInt(16 << 20), NewInt(depth), NewBool(fail)})
				t.Logf("popped receivers retain %d bytes", held)
				if limit := int64(2 << 20); held > limit {
					t.Errorf("popped receivers retain %d bytes, want less than %d", held, limit)
				}
			})
		}
	}
}

func TestCompletedRescuesReleasePayloads(t *testing.T) {
	const source = `def fail(bytes, depth)
  begin
    raise("x" * bytes)
  rescue
    if depth > 0
      fail(0, depth - 1)
    end
    nil
  end
  nil
end
def run(bytes, depth)
  measure_heap()
  fail(bytes, depth)
  measure_heap()
  19
end`
	for _, depth := range []int64{0, 16} {
		t.Run(fmt.Sprintf("depth=%d", depth), func(t *testing.T) {
			held := returnedFrameHeapBytes(t, source, []Value{NewInt(4 << 20), NewInt(depth)})
			t.Logf("completed rescues retain %d bytes", held)
			if limit := int64(1 << 20); held > limit {
				t.Errorf("completed rescues retain %d bytes, want less than %d", held, limit)
			}
		})
	}
}
