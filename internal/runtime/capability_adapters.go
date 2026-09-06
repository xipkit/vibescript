package runtime

import (
	"fmt"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/internal/jobqueueoptions"

	"github.com/mgomes/vibescript/vibes/capability/contextcap"
	"github.com/mgomes/vibescript/vibes/capability/db"
	"github.com/mgomes/vibescript/vibes/capability/events"
	"github.com/mgomes/vibescript/vibes/capability/jobqueue"
)

// Internal aliases for jobqueue capability types so runtime code (and
// tests) can keep referring to short names that match the public vibes
// facade.
type (
	JobQueue               = jobqueue.JobQueue
	JobQueueWithRetry      = jobqueue.JobQueueWithRetry
	JobQueueJob            = jobqueue.JobQueueJob
	JobQueueEnqueueOptions = jobqueue.JobQueueEnqueueOptions
	JobQueueRetryRequest   = jobqueue.JobQueueRetryRequest
)

// jobQueueCapability adapts a *jobqueue.Capability into the
// CapabilityAdapter and CapabilityContractProvider interfaces consumed
// by the runtime. Bind exposes enqueue (and retry when supported) as
// builtin methods under the capability name.
type jobQueueCapability struct {
	inner *jobqueue.Capability
}

// NewJobQueueCapability constructs a CapabilityAdapter that delegates to a
// *jobqueue.Capability. It is the runtime-facing entry point used by the
// vibes facade.
func NewJobQueueCapability(name string, impl JobQueue) (CapabilityAdapter, error) {
	inner, err := jobqueue.NewCapability(name, impl)
	if err != nil {
		return nil, err
	}
	return &jobQueueCapability{inner: inner}, nil
}

// MustNewJobQueueCapability is the panicking variant of NewJobQueueCapability.
func MustNewJobQueueCapability(name string, impl JobQueue) CapabilityAdapter {
	cap, err := NewJobQueueCapability(name, impl)
	if err != nil {
		panic(err)
	}
	return cap
}

func (c *jobQueueCapability) Bind(binding CapabilityBinding) (map[string]Value, error) {
	name := c.inner.Name
	methods := map[string]Value{
		// The first-party adapters clone everything both ways -- what they
		// keep, what they return, what they yield -- and never write a
		// script wrapper, so each declares non-mutation and non-retention:
		// dispatch then skips the isolation copy, the return detach, and
		// the CallBlock boundary copies, leaving the adapter's own clone as
		// the only one.
		"enqueue": DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".enqueue", c.callEnqueue))),
	}
	if c.inner.HasRetry() {
		methods["retry"] = DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".retry", c.callRetry)))
	}
	return map[string]Value{name: NewObject(methods)}, nil
}

func (c *jobQueueCapability) callEnqueue(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	name := c.inner.Name
	method := name + ".enqueue"
	budget, reservation, err := newCapabilityDataBudget(exec, receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	defer reservation.release()
	if !exec.capabilityArgsValidated(method) {
		if err := c.validateEnqueueContractArgsWithBudget(budget, args, kwargs, block); err != nil {
			return NewNil(), err
		}
	}

	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{PreserveObjectTags: true})
	options, err := jobqueueoptions.Parse(name, kwargs, budget, cloner, false)
	if err != nil {
		return NewNil(), err
	}
	payload, err := cloner.Hash(method+" payload", args[1])
	if err != nil {
		return NewNil(), err
	}
	job := jobqueue.JobQueueJob{
		Name:    args[0].String(),
		Payload: payload,
		Options: jobqueue.JobQueueEnqueueOptions{Delay: options.Delay, Key: options.Key, Kwargs: options.Kwargs},
	}

	result, err := c.inner.Queue.Enqueue(exec.Context(), job)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	cloned, err := cloneCapabilityResult(budget, method, result)
	if err != nil {
		return NewNil(), err
	}
	// The clone above validated and isolated the result; record the internal
	// proof so the dispatcher's contract check does not validate it twice.
	exec.markValidatedCapabilityReturn(method, cloned)
	return cloned, nil
}

func (c *jobQueueCapability) callRetry(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	name := c.inner.Name
	if c.inner.Retry == nil {
		return NewNil(), fmt.Errorf("%s.retry is not supported", name)
	}
	method := name + ".retry"
	budget, reservation, err := newCapabilityDataBudget(exec, receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	defer reservation.release()
	if !exec.capabilityArgsValidated(method) {
		if err := c.validateRetryContractArgsWithBudget(budget, args, kwargs, block); err != nil {
			return NewNil(), err
		}
	}

	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{PreserveObjectTags: true})
	var positional map[string]Value
	if len(args) > 1 {
		positional, err = cloner.Hash(method+" options", args[1])
		if err != nil {
			return NewNil(), err
		}
	}
	extra, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return NewNil(), err
	}
	if err := budget.ReserveMap(saturatingAdd(len(positional), len(extra))); err != nil {
		return NewNil(), err
	}
	options := make(map[string]Value, len(positional)+len(extra))
	for _, entries := range []map[string]Value{positional, extra} {
		for key, item := range entries {
			if err := budget.Work(len(key) + 1); err != nil {
				return NewNil(), err
			}
			options[key] = item
		}
	}

	req := jobqueue.JobQueueRetryRequest{JobID: args[0].String(), Options: options}
	result, err := c.inner.Retry.Retry(exec.Context(), req)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	cloned, err := cloneCapabilityResult(budget, method, result)
	if err != nil {
		return NewNil(), err
	}
	// The clone above validated and isolated the result; record the internal
	// proof so the dispatcher's contract check does not validate it twice.
	exec.markValidatedCapabilityReturn(method, cloned)
	return cloned, nil
}

func (c *jobQueueCapability) CapabilityContracts() map[string]CapabilityMethodContract {
	name := c.inner.Name
	contracts := map[string]CapabilityMethodContract{
		name + ".enqueue": {
			ValidateArgs:   c.validateEnqueueContractArgs,
			ValidateReturn: capabilityValidateAnyReturn(name + ".enqueue"),
		},
	}
	if c.inner.HasRetry() {
		contracts[name+".retry"] = CapabilityMethodContract{
			ValidateArgs:   c.validateRetryContractArgs,
			ValidateReturn: capabilityValidateAnyReturn(name + ".retry"),
		}
	}
	return contracts
}

func (c *jobQueueCapability) validateEnqueueContractArgs(args []Value, kwargs map[string]Value, block Value) error {
	return c.validateEnqueueContractArgsWithBudget(nil, args, kwargs, block)
}

func (c *jobQueueCapability) validateEnqueueContractArgsWithBudget(budget *capabilitydata.Budget, args []Value, kwargs map[string]Value, block Value) error {
	method := c.inner.Name + ".enqueue"

	if len(args) != 2 {
		return fmt.Errorf("%s expects job name and payload", method)
	}
	if !block.IsNil() {
		return fmt.Errorf("%s does not accept blocks", method)
	}

	jobNameVal := args[0]
	switch jobNameVal.Kind() {
	case KindString, KindSymbol:
		// supported
	default:
		return fmt.Errorf("%s expects job name as string or symbol", method)
	}

	validator := capabilitydata.NewValidator(budget)
	if err := validateCapabilityTypedValueWithValidator(validator, method+" payload", args[1], capabilityTypeHash); err != nil {
		return err
	}
	return validator.Kwargs(method, kwargs)
}

func (c *jobQueueCapability) validateRetryContractArgs(args []Value, kwargs map[string]Value, block Value) error {
	return c.validateRetryContractArgsWithBudget(nil, args, kwargs, block)
}

func (c *jobQueueCapability) validateRetryContractArgsWithBudget(budget *capabilitydata.Budget, args []Value, kwargs map[string]Value, block Value) error {
	method := c.inner.Name + ".retry"

	if len(args) < 1 || len(args) > 2 {
		return fmt.Errorf("%s expects job id and optional options hash", method)
	}
	if !block.IsNil() {
		return fmt.Errorf("%s does not accept blocks", method)
	}

	idVal := args[0]
	if idVal.Kind() != KindString {
		return fmt.Errorf("%s expects job id string", method)
	}

	validator := capabilitydata.NewValidator(budget)
	if len(args) == 2 {
		if err := validateCapabilityTypedValueWithValidator(validator, method+" options", args[1], capabilityTypeHash); err != nil {
			return err
		}
	}
	return validator.Kwargs(method, kwargs)
}

// ContextCapabilityResolver is an internal alias for contextcap.Resolver
// so runtime code (and tests) can keep using the short name that matches
// the public vibes facade.
type ContextCapabilityResolver = contextcap.Resolver

// NewContextCapability constructs a data-only context capability adapter
// that bridges a contextcap.Resolver into the runtime CapabilityAdapter
// interface. The vibes facade re-exports this entry point under the same
// name.
func NewContextCapability(name string, resolver ContextCapabilityResolver) (CapabilityAdapter, error) {
	inner, err := contextcap.NewCapability(name, resolver)
	if err != nil {
		return nil, err
	}
	return &contextCapabilityAdapter{inner: inner}, nil
}

// MustNewContextCapability is the panicking variant of NewContextCapability.
func MustNewContextCapability(name string, resolver ContextCapabilityResolver) CapabilityAdapter {
	cap, err := NewContextCapability(name, resolver)
	if err != nil {
		panic(err)
	}
	return cap
}

// contextCapabilityAdapter bridges contextcap.Capability into vibes's
// CapabilityAdapter interface, which is anchored on the vibes-side
// CapabilityBinding type and cannot live in the carved subpackage.
type contextCapabilityAdapter struct {
	inner *contextcap.Capability
}

func (a *contextCapabilityAdapter) Bind(binding CapabilityBinding) (map[string]Value, error) {
	return a.inner.Bind(binding.Context)
}

func (a *contextCapabilityAdapter) bindWithExecution(exec *Execution, binding CapabilityBinding) (map[string]Value, error) {
	budget, reservation, err := newCapabilityDataBudget(exec, NewNil(), nil, nil, NewNil())
	if err != nil {
		return nil, err
	}
	defer reservation.release()
	return a.inner.Bind(capabilitydata.WithBudget(binding.Context, budget))
}

// These adapters enforce their public contracts inside the budgeted call, as
// DB does. Keep the standalone validators available to direct embedders.
func (c *jobQueueCapability) runtimeCapabilityContracts() map[string]CapabilityMethodContract {
	return nil
}

func (c *eventsCapability) runtimeCapabilityContracts() map[string]CapabilityMethodContract {
	return nil
}

// Internal aliases for db capability types so runtime code (and tests)
// can keep referring to short names that match the public vibes facade.
type (
	Database        = db.Database
	DatabaseReader  = db.DatabaseReader
	DatabaseWriter  = db.DatabaseWriter
	DBFindRequest   = db.DBFindRequest
	DBQueryRequest  = db.DBQueryRequest
	DBUpdateRequest = db.DBUpdateRequest
	DBSumRequest    = db.DBSumRequest
	DBEachRequest   = db.DBEachRequest
)

// NewDBCapability constructs a database capability adapter bound to the
// provided script-facing name. The vibes facade re-exports this entry
// point under the same name.
func NewDBCapability(name string, impl Database) (CapabilityAdapter, error) {
	cap, err := db.NewCapability(name, impl)
	if err != nil {
		return nil, err
	}
	return &dbCapabilityAdapter{cap: cap}, nil
}

// MustNewDBCapability is the panicking variant of NewDBCapability.
func MustNewDBCapability(name string, impl Database) CapabilityAdapter {
	cap, err := NewDBCapability(name, impl)
	if err != nil {
		panic(err)
	}
	return cap
}

type dbCapabilityAdapter struct {
	cap *db.Capability
}

func (a *dbCapabilityAdapter) Bind(_ CapabilityBinding) (map[string]Value, error) {
	name := a.cap.Name()
	contracts := a.cap.Contracts()
	methods := map[string]Value{
		"find":   DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".find", a.wrapCall(name+".find", a.cap.CallFind, contracts[name+".find"].CallValidated)))),
		"query":  DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".query", a.wrapCall(name+".query", a.cap.CallQuery, contracts[name+".query"].CallValidated)))),
		"update": DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".update", a.wrapCall(name+".update", a.cap.CallUpdate, contracts[name+".update"].CallValidated)))),
		"sum":    DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".sum", a.wrapCall(name+".sum", a.cap.CallSum, contracts[name+".sum"].CallValidated)))),
		"each":   DeclareNonMutating(DeclareNonRetaining(NewBuiltin(name+".each", a.wrapCall(name+".each", a.cap.CallEach, contracts[name+".each"].CallValidated)))),
	}
	return map[string]Value{name: NewObject(methods)}, nil
}

func (a *dbCapabilityAdapter) CapabilityContracts() map[string]CapabilityMethodContract {
	// DB methods validate arguments and clone returns inside the adapter wrapper,
	// so there are no runtime-level contracts to bind or rescan.
	return nil
}

func (a *dbCapabilityAdapter) wrapCall(method string, fn, validatedFn func(db.ExecutionContext, []Value, map[string]Value, Value) (Value, error)) BuiltinFunc {
	return func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
		budget, reservation, err := newCapabilityDataBudget(exec, receiver, args, kwargs, block)
		if err != nil {
			return NewNil(), err
		}
		defer reservation.release()
		call := fn
		if validatedFn != nil && exec.capabilityArgsValidated(method) {
			call = validatedFn
		}
		result, err := call(&capabilityDataExecution{Execution: exec, budget: budget}, args, kwargs, block)
		if err != nil {
			return result, err
		}
		// Every db call validates and isolates its result from host state;
		// the proof lets the dispatcher skip detaching it a second time.
		exec.markValidatedCapabilityReturn(method, result)
		return result, nil
	}
}

// Internal aliases for events capability types so runtime code (and tests)
// can keep referring to short names that match the public vibes facade.
type (
	EventPublisher      = events.Publisher
	EventPublishRequest = events.PublishRequest
)

// NewEventsCapability constructs a CapabilityAdapter that delegates to a
// *events.Capability. The vibes facade re-exports this entry point under
// the same name.
func NewEventsCapability(name string, publisher EventPublisher) (CapabilityAdapter, error) {
	inner, err := events.NewCapability(name, publisher)
	if err != nil {
		return nil, err
	}
	return &eventsCapability{inner: inner}, nil
}

// MustNewEventsCapability is the panicking variant of NewEventsCapability.
func MustNewEventsCapability(name string, publisher EventPublisher) CapabilityAdapter {
	cap, err := NewEventsCapability(name, publisher)
	if err != nil {
		panic(err)
	}
	return cap
}

// eventsCapability bridges an *events.Capability into the runtime by
// implementing CapabilityAdapter and CapabilityContractProvider.
type eventsCapability struct {
	inner *events.Capability
}

func (c *eventsCapability) CapabilityContracts() map[string]CapabilityMethodContract {
	method := c.inner.PublishMethodName()
	return map[string]CapabilityMethodContract{
		method: {
			ValidateArgs:   c.validatePublishArgs,
			ValidateReturn: c.inner.ValidatePublishReturn,
		},
	}
}

func (c *eventsCapability) Bind(binding CapabilityBinding) (map[string]Value, error) {
	methods := map[string]Value{
		"publish": DeclareNonMutating(DeclareNonRetaining(NewBuiltin(c.inner.PublishMethodName(), c.callPublish))),
	}
	return map[string]Value{c.inner.Name: NewObject(methods)}, nil
}

func (c *eventsCapability) callPublish(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	method := c.inner.PublishMethodName()
	budget, reservation, err := newCapabilityDataBudget(exec, receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	defer reservation.release()
	ctx := capabilitydata.WithBudget(exec.Context(), budget)
	var result Value
	if exec.capabilityArgsValidated(method) {
		result, err = c.inner.PublishValidated(ctx, args, kwargs, !block.IsNil())
	} else {
		result, err = c.inner.Publish(ctx, args, kwargs, !block.IsNil())
	}
	if err != nil {
		return NewNil(), err
	}
	// Both publish paths validate and isolate the host's return value; record
	// the internal proof so the dispatcher does not validate the result twice.
	exec.markValidatedCapabilityReturn(method, result)
	return result, nil
}

func (c *eventsCapability) validatePublishArgs(args []Value, kwargs map[string]Value, block Value) error {
	return c.inner.ValidatePublishArgs(args, kwargs, !block.IsNil())
}

var (
	_ CapabilityAdapter = (*contextCapabilityAdapter)(nil)
	_ CapabilityAdapter = (*dbCapabilityAdapter)(nil)
	_ CapabilityAdapter = (*eventsCapability)(nil)
	_ CapabilityAdapter = (*jobQueueCapability)(nil)
)

var (
	_ CapabilityContractProvider = (*dbCapabilityAdapter)(nil)
	_ CapabilityContractProvider = (*eventsCapability)(nil)
	_ CapabilityContractProvider = (*jobQueueCapability)(nil)
)
