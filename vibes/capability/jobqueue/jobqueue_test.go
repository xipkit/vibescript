package jobqueue

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/mgomes/vibescript/vibes/internal/capabilitycontract"
	"github.com/mgomes/vibescript/vibes/value"
)

// queueStub records enqueue and retry calls so tests can assert on the
// requests the capability forwarded to the host.
type queueStub struct {
	enqueueCalls []JobQueueJob
	enqueueRes   value.Value
	enqueueErr   error
}

var _ JobQueue = (*queueStub)(nil)

func (s *queueStub) Enqueue(_ context.Context, job JobQueueJob) (value.Value, error) {
	s.enqueueCalls = append(s.enqueueCalls, job)
	if s.enqueueErr != nil {
		return value.NewNil(), s.enqueueErr
	}
	return s.enqueueRes, nil
}

// retryQueueStub also satisfies JobQueueWithRetry so HasRetry is exercised.
type retryQueueStub struct {
	queueStub
}

var _ JobQueueWithRetry = (*retryQueueStub)(nil)

func (s *retryQueueStub) Retry(_ context.Context, _ JobQueueRetryRequest) (value.Value, error) {
	return value.NewNil(), nil
}

func TestNewCapabilityRejectsInvalidArguments(t *testing.T) {
	t.Parallel()

	stub := &queueStub{}
	var nilQueue JobQueue
	var typedNil *queueStub

	tests := []struct {
		name    string
		capName string
		queue   JobQueue
		wantErr string
	}{
		{name: "empty_name", capName: "", queue: stub, wantErr: "name must be non-empty"},
		{name: "nil_interface", capName: "jobs", queue: nilQueue, wantErr: "requires a non-nil implementation"},
		{name: "typed_nil", capName: "jobs", queue: typedNil, wantErr: "requires a non-nil implementation"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := NewCapability(tc.capName, tc.queue)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("expected error containing %q, got %v", tc.wantErr, err)
			}
		})
	}
}

func TestMustNewCapabilityPanicsOnInvalidArguments(t *testing.T) {
	t.Parallel()
	defer func() {
		if r := recover(); r == nil {
			t.Fatal("expected panic on invalid arguments")
		}
	}()
	_ = MustNewCapability("", nil)
}

func TestCapabilityHasRetryDetection(t *testing.T) {
	t.Parallel()

	plain := MustNewCapability("jobs", &queueStub{})
	if plain.HasRetry() {
		t.Fatal("plain queue must not report retry support")
	}
	if plain.Retry != nil {
		t.Fatal("plain queue must not bind a retry implementation")
	}

	withRetry := MustNewCapability("jobs", &retryQueueStub{})
	if !withRetry.HasRetry() {
		t.Fatal("retry queue must report retry support")
	}
	if withRetry.Retry == nil {
		t.Fatal("retry queue must bind a retry implementation")
	}
}

func TestParseEnqueueOptionsEmptyReturnsZero(t *testing.T) {
	t.Parallel()

	opts, err := ParseEnqueueOptions("jobs", nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if diff := cmp.Diff(JobQueueEnqueueOptions{}, opts); diff != "" {
		t.Fatalf("unexpected options (-want +got):\n%s", diff)
	}
}

func TestParseEnqueueOptionsParsesDelayKeyAndExtra(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		kwargs    map[string]value.Value
		wantDelay time.Duration
		wantKey   string
		wantExtra map[string]value.Value
	}{
		{
			name:      "duration_delay",
			kwargs:    map[string]value.Value{"delay": value.NewDuration(value.DurationFromSeconds(90))},
			wantDelay: 90 * time.Second,
		},
		{
			name:      "integer_seconds_delay",
			kwargs:    map[string]value.Value{"delay": value.NewInt(30)},
			wantDelay: 30 * time.Second,
		},
		{
			name:      "float_seconds_delay",
			kwargs:    map[string]value.Value{"delay": value.NewFloat(45.0)},
			wantDelay: 45 * time.Second,
		},
		{
			name:    "key",
			kwargs:  map[string]value.Value{"key": value.NewString("dedupe-1")},
			wantKey: "dedupe-1",
		},
		{
			name: "extra_kwargs_cloned",
			kwargs: map[string]value.Value{
				"priority": value.NewInt(5),
				"tags":     value.NewArray([]value.Value{value.NewString("a")}),
			},
			wantExtra: map[string]value.Value{
				"priority": value.NewInt(5),
				"tags":     value.NewArray([]value.Value{value.NewString("a")}),
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			opts, err := ParseEnqueueOptions("jobs", tc.kwargs)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tc.wantDelay != 0 {
				if opts.Delay == nil || *opts.Delay != tc.wantDelay {
					t.Fatalf("delay = %v, want %v", opts.Delay, tc.wantDelay)
				}
			} else if opts.Delay != nil {
				t.Fatalf("unexpected delay: %v", *opts.Delay)
			}
			if tc.wantKey != "" {
				if opts.Key == nil || *opts.Key != tc.wantKey {
					t.Fatalf("key = %v, want %q", opts.Key, tc.wantKey)
				}
			} else if opts.Key != nil {
				t.Fatalf("unexpected key: %v", *opts.Key)
			}
			if diff := cmp.Diff(tc.wantExtra, opts.Kwargs, cmp.Comparer(valuesEqual)); diff != "" {
				t.Fatalf("unexpected extra kwargs (-want +got):\n%s", diff)
			}
		})
	}
}

func TestParseEnqueueOptionsPreservesHashInsertionOrder(t *testing.T) {
	t.Parallel()

	original := value.NewHash(map[string]value.Value{})
	for i, key := range []string{"b", "a"} {
		if err := original.HashSet(value.NewString(key), value.NewInt(int64(i+1))); err != nil {
			t.Fatalf("HashSet(%s) error = %v, want nil", key, err)
		}
	}

	options, err := ParseEnqueueOptions("jobs", map[string]value.Value{"data": original})
	if err != nil {
		t.Fatal(err)
	}
	cloned := options.Kwargs["data"]
	entries := cloned.HashEntries()
	if len(entries) != 2 {
		t.Fatalf("deepCloneValue key count = %d, want 2", len(entries))
	}
	if got, want := entries[0].Key.String(), "b"; got != want {
		t.Fatalf("deepCloneValue key[0] = %q, want %q", got, want)
	}
	if got, want := entries[1].Key.String(), "a"; got != want {
		t.Fatalf("deepCloneValue key[1] = %q, want %q", got, want)
	}
}

func TestParseEnqueueOptionsClonesExtraKwargs(t *testing.T) {
	t.Parallel()

	inner := map[string]value.Value{"name": value.NewString("original")}
	kwargs := map[string]value.Value{"meta": value.NewHash(inner)}

	opts, err := ParseEnqueueOptions("jobs", kwargs)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	opts.Kwargs["meta"].Hash()["name"] = value.NewString("mutated")
	if inner["name"].String() != "original" {
		t.Fatalf("clone leaked into caller-owned map: %#v", inner)
	}
}

func TestParseEnqueueOptionsRejectsInvalidValues(t *testing.T) {
	t.Parallel()

	callable := value.NewValue(value.KindFunction, struct{}{})

	cyclic := map[string]value.Value{}
	cyclic["self"] = value.NewHash(cyclic)

	nestedCallable := map[string]value.Value{
		"steps": value.NewArray([]value.Value{
			value.NewHash(map[string]value.Value{"run": callable}),
		}),
	}

	tests := []struct {
		name    string
		kwargs  map[string]value.Value
		wantErr string
	}{
		{
			name:    "negative_delay",
			kwargs:  map[string]value.Value{"delay": value.NewInt(-1)},
			wantErr: "jobs.enqueue delay must be non-negative",
		},
		{
			name:    "non_numeric_delay",
			kwargs:  map[string]value.Value{"delay": value.NewString("soon")},
			wantErr: "jobs.enqueue delay must be duration or numeric seconds",
		},
		{
			name:    "non_string_key",
			kwargs:  map[string]value.Value{"key": value.NewInt(7)},
			wantErr: "jobs.enqueue key must be a string",
		},
		{
			name:    "empty_key",
			kwargs:  map[string]value.Value{"key": value.NewString("")},
			wantErr: "jobs.enqueue key must be non-empty",
		},
		{
			name:    "callable_extra_kwarg",
			kwargs:  map[string]value.Value{"callback": callable},
			wantErr: "jobs.enqueue keyword callback must be data-only",
		},
		{
			name:    "nested_callable_extra_kwarg",
			kwargs:  map[string]value.Value{"plan": value.NewHash(nestedCallable)},
			wantErr: "jobs.enqueue keyword plan must be data-only",
		},
		{
			name:    "cyclic_extra_kwarg",
			kwargs:  map[string]value.Value{"loop": value.NewHash(cyclic)},
			wantErr: "jobs.enqueue keyword loop must not contain cyclic references",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := ParseEnqueueOptions("jobs", tc.kwargs)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("expected error containing %q, got %v", tc.wantErr, err)
			}
		})
	}
}

func TestParseEnqueueOptionsRejectsDeepTraversal(t *testing.T) {
	t.Parallel()

	_, err := ParseEnqueueOptions("jobs", map[string]value.Value{
		"meta": deepJobQueueArray(capabilitycontract.MaxDataOnlyTraversalDepth + 1),
	})
	if err == nil {
		t.Fatal("ParseEnqueueOptions deep traversal err = nil, want limit error")
	}
	limit, ok := err.(interface{ LimitError() bool })
	if !ok || !limit.LimitError() {
		t.Fatalf("ParseEnqueueOptions deep traversal err = %T %v, want LimitError marker", err, err)
	}
	if !strings.Contains(err.Error(), "jobs.enqueue keyword meta exceeds maximum depth") {
		t.Fatalf("ParseEnqueueOptions deep traversal err = %v, want depth error", err)
	}
}

func TestParseEnqueueOptionsValidatedSkipsDataOnlyWalk(t *testing.T) {
	t.Parallel()

	// The validated fast path trusts callers that already enforced the
	// data-only contract, so it must not reject a callable extra kwarg.
	callable := value.NewValue(value.KindFunction, struct{}{})
	kwargs := map[string]value.Value{"callback": callable}

	opts, err := ParseEnqueueOptionsValidated("jobs", kwargs)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got := opts.Kwargs["callback"].Kind(); got != value.KindFunction {
		t.Fatalf("expected callable passthrough, got %s", got)
	}

	// The safe path must still reject the same input.
	if _, err := ParseEnqueueOptions("jobs", kwargs); err == nil {
		t.Fatal("expected safe parser to reject callable kwarg")
	}
}

func TestParseEnqueueOptionsValidatedStillValidatesDelayAndKey(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		kwargs  map[string]value.Value
		wantErr string
	}{
		{
			name:    "negative_delay",
			kwargs:  map[string]value.Value{"delay": value.NewInt(-1)},
			wantErr: "jobs.enqueue delay must be non-negative",
		},
		{
			name:    "empty_key",
			kwargs:  map[string]value.Value{"key": value.NewString("")},
			wantErr: "jobs.enqueue key must be non-empty",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := ParseEnqueueOptionsValidated("jobs", tc.kwargs)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("expected error containing %q, got %v", tc.wantErr, err)
			}
		})
	}
}

// valuesEqual compares two data-only values for cmp.Diff, walking arrays
// and hashes recursively. It only needs the kinds the option parser emits.
func valuesEqual(a, b value.Value) bool {
	if a.Kind() != b.Kind() {
		return false
	}
	switch a.Kind() {
	case value.KindArray:
		left, right := a.Array(), b.Array()
		if len(left) != len(right) {
			return false
		}
		for i := range left {
			if !valuesEqual(left[i], right[i]) {
				return false
			}
		}
		return true
	case value.KindHash, value.KindObject:
		left, right := a.Hash(), b.Hash()
		if len(left) != len(right) {
			return false
		}
		for k, lv := range left {
			rv, ok := right[k]
			if !ok || !valuesEqual(lv, rv) {
				return false
			}
		}
		return true
	default:
		return a.Equal(b)
	}
}

func deepJobQueueArray(depth int) value.Value {
	val := value.NewString("leaf")
	for range depth {
		val = value.NewArray([]value.Value{val})
	}
	return val
}
