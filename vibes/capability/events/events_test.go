package events

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

type stubPublisher struct {
	calls   []PublishRequest
	ctxs    []context.Context
	result  value.Value
	failure error
	cancel  context.CancelFunc
}

var _ Publisher = (*stubPublisher)(nil)

func (s *stubPublisher) Publish(ctx context.Context, req PublishRequest) (value.Value, error) {
	s.calls = append(s.calls, req)
	s.ctxs = append(s.ctxs, ctx)
	if s.failure != nil {
		return value.NewNil(), s.failure
	}
	if s.cancel != nil {
		s.cancel()
	}
	return s.result, nil
}

type benchmarkPublisher struct {
	result value.Value
}

func (p benchmarkPublisher) Publish(context.Context, PublishRequest) (value.Value, error) {
	return p.result, nil
}

func TestNewCapabilityRejectsInvalidArguments(t *testing.T) {
	t.Parallel()

	stub := &stubPublisher{}
	var nilPublisher Publisher
	var typedNil *stubPublisher

	tests := []struct {
		name      string
		capName   string
		publisher Publisher
		wantErr   string
	}{
		{name: "empty_name", capName: "", publisher: stub, wantErr: "name must be non-empty"},
		{name: "nil_interface", capName: "events", publisher: nilPublisher, wantErr: "requires a non-nil implementation"},
		{name: "typed_nil", capName: "events", publisher: typedNil, wantErr: "requires a non-nil implementation"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := NewCapability(tc.capName, tc.publisher)
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

func TestCapabilityPublishCallsHostAndClonesResult(t *testing.T) {
	t.Parallel()
	stub := &stubPublisher{
		result: value.NewHash(map[string]value.Value{
			"meta": value.NewHash(map[string]value.Value{
				"trace": value.NewString("host"),
			}),
		}),
	}
	cap := MustNewCapability("events", stub)

	args := []value.Value{
		value.NewString("topic"),
		value.NewHash(map[string]value.Value{"id": value.NewString("p-1")}),
	}
	result, err := cap.Publish(context.Background(), args, map[string]value.Value{"trace": value.NewString("abc")}, false)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got := result.Hash()["meta"].Hash()["trace"].String(); got != "host" {
		t.Fatalf("unexpected return: %s", got)
	}

	result.Hash()["meta"].Hash()["trace"] = value.NewString("mutated")
	if stub.result.Hash()["meta"].Hash()["trace"].String() != "host" {
		t.Fatalf("clone leaked host state")
	}

	if len(stub.calls) != 1 {
		t.Fatalf("expected 1 publish call, got %d", len(stub.calls))
	}
	if stub.calls[0].Topic != "topic" {
		t.Fatalf("unexpected topic: %s", stub.calls[0].Topic)
	}
	if stub.calls[0].Payload["id"].String() != "p-1" {
		t.Fatalf("unexpected payload: %#v", stub.calls[0].Payload)
	}
	if stub.calls[0].Options["trace"].String() != "abc" {
		t.Fatalf("unexpected options: %#v", stub.calls[0].Options)
	}
}

func TestCapabilityPublishStopsAfterHostCancellation(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithCancel(context.Background())
	stub := &stubPublisher{
		cancel: cancel,
		result: value.NewObject(map[string]value.Value{
			"fn": value.NewValue(value.KindBlock, struct{}{}),
		}),
	}
	cap := MustNewCapability("events", stub)

	args := []value.Value{value.NewString("topic"), value.NewHash(nil)}
	_, err := cap.Publish(ctx, args, nil, false)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Publish canceled by host error = %v, want context.Canceled", err)
	}
}

func TestValidatePublishArgsRejectsInvalid(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		args    []value.Value
		kwargs  map[string]value.Value
		block   bool
		wantErr string
	}{
		{
			name:    "no_args",
			args:    nil,
			wantErr: "expects topic and payload",
		},
		{
			name:    "payload_wrong_type",
			args:    []value.Value{value.NewString("topic"), value.NewInt(42)},
			wantErr: "expected hash, got int",
		},
		{
			name:    "block_provided",
			args:    []value.Value{value.NewString("topic"), value.NewHash(nil)},
			block:   true,
			wantErr: "does not accept blocks",
		},
		{
			name:    "empty_topic",
			args:    []value.Value{value.NewString(""), value.NewHash(nil)},
			wantErr: "non-empty string or symbol",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			cap := MustNewCapability("events", &stubPublisher{result: value.NewNil()})
			err := cap.ValidatePublishArgs(tc.args, tc.kwargs, tc.block)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("expected error containing %q, got %v", tc.wantErr, err)
			}
		})
	}
}

func TestCapabilityPublishPropagatesHostError(t *testing.T) {
	t.Parallel()
	boom := errors.New("boom")
	stub := &stubPublisher{failure: boom}
	cap := MustNewCapability("events", stub)

	args := []value.Value{value.NewString("topic"), value.NewHash(nil)}
	_, err := cap.Publish(context.Background(), args, nil, false)
	if !errors.Is(err, boom) {
		t.Fatalf("expected wrapped host error, got %v", err)
	}
}

func TestCapabilityPublishRejectsCyclicPayload(t *testing.T) {
	t.Parallel()
	stub := &stubPublisher{result: value.NewNil()}
	cap := MustNewCapability("events", stub)

	cyclic := map[string]value.Value{}
	cyclic["self"] = value.NewHash(cyclic)
	args := []value.Value{value.NewString("topic"), value.NewHash(cyclic)}

	_, err := cap.Publish(context.Background(), args, nil, false)
	if err == nil || !strings.Contains(err.Error(), "must not contain cyclic references") {
		t.Fatalf("expected cycle rejection, got %v", err)
	}
}

func TestPublishPreservesReturnedHashInsertionOrder(t *testing.T) {
	t.Parallel()

	original := value.NewHash(map[string]value.Value{})
	for i, key := range []string{"b", "a"} {
		if err := original.HashSet(value.NewString(key), value.NewInt(int64(i+1))); err != nil {
			t.Fatalf("HashSet(%s) error = %v, want nil", key, err)
		}
	}

	cap := MustNewCapability("events", &stubPublisher{result: original})
	cloned, err := cap.Publish(context.Background(), []value.Value{value.NewString("topic"), value.NewHash(nil)}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	entries := cloned.HashEntries()
	if len(entries) != 2 {
		t.Fatalf("deepClone key count = %d, want 2", len(entries))
	}
	if got, want := entries[0].Key.String(), "b"; got != want {
		t.Fatalf("deepClone key[0] = %q, want %q", got, want)
	}
	if got, want := entries[1].Key.String(), "a"; got != want {
		t.Fatalf("deepClone key[1] = %q, want %q", got, want)
	}
}

func BenchmarkCapabilityPublishValidated(b *testing.B) {
	result := value.NewHash(map[string]value.Value{
		"meta": value.NewHash(map[string]value.Value{
			"trace": value.NewString("host"),
			"tags": value.NewArray([]value.Value{
				value.NewString("alpha"),
				value.NewString("beta"),
			}),
		}),
	})
	cap := MustNewCapability("events", benchmarkPublisher{result: result})
	args := []value.Value{
		value.NewString("topic"),
		value.NewHash(map[string]value.Value{"id": value.NewString("p-1")}),
	}
	kwargs := map[string]value.Value{"trace": value.NewString("abc")}

	b.ReportAllocs()
	for range b.N {
		got, err := cap.PublishValidated(context.Background(), args, kwargs, false)
		if err != nil {
			b.Fatalf("PublishValidated() error = %v", err)
		}
		if got.Kind() != value.KindHash {
			b.Fatalf("PublishValidated() = %s, want hash", got.Kind())
		}
	}
}
