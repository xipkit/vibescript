// Package jobqueue defines the host-facing contract for the job-queue
// capability that Vibescript exposes to scripts. The runtime wraps a
// *Capability with a script-visible adapter; embedders implement
// JobQueue (and optionally JobQueueWithRetry) to back the methods.
package jobqueue

import (
	"context"
	"fmt"
	"reflect"
	"time"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/internal/jobqueueoptions"
	"github.com/mgomes/vibescript/vibes/value"
)

// JobQueue exposes queue functionality to scripts via strongly-typed adapters.
type JobQueue interface {
	Enqueue(ctx context.Context, job JobQueueJob) (value.Value, error)
}

// JobQueueWithRetry extends JobQueue with a retry operation.
type JobQueueWithRetry interface {
	JobQueue
	Retry(ctx context.Context, req JobQueueRetryRequest) (value.Value, error)
}

// JobQueueJob captures a job invocation from script code.
type JobQueueJob struct {
	Name    string
	Payload map[string]value.Value
	Options JobQueueEnqueueOptions
}

// JobQueueEnqueueOptions represents keyword arguments supplied to enqueue.
type JobQueueEnqueueOptions struct {
	Delay  *time.Duration
	Key    *string
	Kwargs map[string]value.Value
}

// JobQueueRetryRequest captures retry invocations.
type JobQueueRetryRequest struct {
	JobID   string
	Options map[string]value.Value
}

// Capability binds a host JobQueue implementation under a script-visible
// name. The vibes package wraps it in a CapabilityAdapter; embedders
// construct one via NewCapability.
type Capability struct {
	Name  string
	Queue JobQueue
	Retry JobQueueWithRetry
}

// NewCapability validates the inputs and returns a bound Capability. It
// returns an error when name is empty or when queue is a nil
// implementation (typed or untyped).
func NewCapability(name string, queue JobQueue) (*Capability, error) {
	if name == "" {
		return nil, fmt.Errorf("vibes: job queue capability name must be non-empty")
	}
	if isNilImpl(queue) {
		return nil, fmt.Errorf("vibes: job queue capability requires a non-nil implementation")
	}
	cap := &Capability{Name: name, Queue: queue}
	if retry, ok := queue.(JobQueueWithRetry); ok {
		cap.Retry = retry
	}
	return cap, nil
}

// MustNewCapability is the panicking variant of NewCapability.
func MustNewCapability(name string, queue JobQueue) *Capability {
	cap, err := NewCapability(name, queue)
	if err != nil {
		panic(err)
	}
	return cap
}

// HasRetry reports whether the bound implementation supports retry.
func (c *Capability) HasRetry() bool { return c.Retry != nil }

// ParseEnqueueOptions converts kwargs received from a script into a
// structured JobQueueEnqueueOptions value. It is the safe public entry
// point: every extra keyword is checked to be data-only (no callables)
// and acyclic before it is cloned into the returned options, so direct
// embedders cannot smuggle a runtime-only value into the host. The name
// is used for error messages so they line up with the script-visible
// capability name.
func ParseEnqueueOptions(name string, kwargs map[string]value.Value) (JobQueueEnqueueOptions, error) {
	return parseEnqueueOptions(name, kwargs, true)
}

// ParseEnqueueOptionsValidated is the fast path for callers that have
// already enforced the enqueue data-only contract on kwargs (for example
// the runtime adapter, which validates arguments against the capability
// contract before dispatching). It still parses and clones delay, key,
// and extra kwargs, but skips the redundant data-only/cycle walk so the
// option graph is not traversed twice.
func ParseEnqueueOptionsValidated(name string, kwargs map[string]value.Value) (JobQueueEnqueueOptions, error) {
	return parseEnqueueOptions(name, kwargs, false)
}

func parseEnqueueOptions(name string, kwargs map[string]value.Value, validate bool) (JobQueueEnqueueOptions, error) {
	budget := capabilitydata.NewBudget(context.Background(), nil, nil)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	options, err := jobqueueoptions.Parse(name, kwargs, budget, cloner, validate)
	if err != nil {
		return JobQueueEnqueueOptions{}, err
	}
	return JobQueueEnqueueOptions{Delay: options.Delay, Key: options.Key, Kwargs: options.Kwargs}, nil
}

// isNilImpl reports whether impl is an untyped or typed nil. It is
// duplicated here rather than imported from vibes to keep this package
// free of an import cycle.
func isNilImpl(impl any) bool {
	if impl == nil {
		return true
	}
	val := reflect.ValueOf(impl)
	switch val.Kind() {
	case reflect.Chan, reflect.Func, reflect.Interface, reflect.Map, reflect.Pointer, reflect.Slice:
		return val.IsNil()
	default:
		return false
	}
}
