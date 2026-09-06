// Package events defines the host-facing contract for the events capability
// that Vibescript exposes to scripts. The runtime wraps a *Capability with a
// script-visible adapter; embedders implement Publisher to back events.publish.
package events

import (
	"context"
	"fmt"
	"reflect"
	"strings"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/vibes/value"
)

// Publisher exposes event publication capability methods to scripts.
type Publisher interface {
	Publish(ctx context.Context, req PublishRequest) (value.Value, error)
}

// PublishRequest captures events.publish calls from script code.
type PublishRequest struct {
	Topic   string
	Payload map[string]value.Value
	Options map[string]value.Value
}

// Capability binds a host Publisher implementation under a script-visible
// name. The vibes package wraps it in a CapabilityAdapter; embedders
// construct one via NewCapability.
type Capability struct {
	Name      string
	Publisher Publisher
}

// NewCapability validates the inputs and returns a bound Capability.
func NewCapability(name string, publisher Publisher) (*Capability, error) {
	if name == "" {
		return nil, fmt.Errorf("vibes: events capability name must be non-empty")
	}
	if isNilImpl(publisher) {
		return nil, fmt.Errorf("vibes: events capability requires a non-nil implementation")
	}
	return &Capability{Name: name, Publisher: publisher}, nil
}

// MustNewCapability is the panicking variant of NewCapability.
func MustNewCapability(name string, publisher Publisher) *Capability {
	cap, err := NewCapability(name, publisher)
	if err != nil {
		panic(err)
	}
	return cap
}

// PublishMethodName returns the dotted script-visible method name (for example
// "events.publish") for use in error messages and contract keys.
func (c *Capability) PublishMethodName() string { return c.Name + ".publish" }

// ValidatePublishArgs enforces the events.publish contract on script-supplied
// arguments. The vibes-side adapter wires this into the runtime contract and
// Publish calls it when embedders invoke the capability directly.
func (c *Capability) ValidatePublishArgs(args []value.Value, kwargs map[string]value.Value, blockProvided bool) error {
	return c.validatePublishArgs(nil, args, kwargs, blockProvided)
}

func (c *Capability) validatePublishArgs(budget *capabilitydata.Budget, args []value.Value, kwargs map[string]value.Value, blockProvided bool) error {
	method := c.PublishMethodName()
	if len(args) != 2 {
		return fmt.Errorf("%s expects topic and payload", method)
	}
	if blockProvided {
		return fmt.Errorf("%s does not accept blocks", method)
	}
	if _, err := nameArg(method, "topic", args[0]); err != nil {
		return err
	}
	if args[1].Kind() != value.KindHash && args[1].Kind() != value.KindObject {
		return fmt.Errorf("%s payload expected hash, got %s", method, args[1].Kind())
	}
	validator := capabilitydata.NewValidator(budget)
	if err := validator.Validate(method+" payload", args[1]); err != nil {
		return err
	}
	return validator.Kwargs(method, kwargs)
}

// ValidatePublishReturn enforces the data-only contract on host return values.
// The vibes-side adapter wires this into CapabilityMethodContract.ValidateReturn.
func (c *Capability) ValidatePublishReturn(result value.Value) error {
	return capabilitydata.NewValidator(nil).Validate(c.PublishMethodName()+" return value", result)
}

// Publish runs the full publish path: validates args, builds the
// PublishRequest, delegates to the host Publisher, validates the return value,
// and deep-clones it so the host can't share mutable state with scripts.
func (c *Capability) Publish(ctx context.Context, args []value.Value, kwargs map[string]value.Value, blockProvided bool) (value.Value, error) {
	ctx, budget := capabilitydata.UnpackBudget(ctx)
	if err := c.validatePublishArgs(budget, args, kwargs, blockProvided); err != nil {
		return value.NewNil(), err
	}
	return c.publishValidated(ctx, budget, args, kwargs)
}

// PublishValidated runs events.publish after the runtime has already enforced
// ValidatePublishArgs. Direct embedders should call Publish so invalid script
// arguments are still rejected before the host publisher runs.
func (c *Capability) PublishValidated(ctx context.Context, args []value.Value, kwargs map[string]value.Value, blockProvided bool) (value.Value, error) {
	ctx, budget := capabilitydata.UnpackBudget(ctx)
	return c.publishValidated(ctx, budget, args, kwargs)
}

func (c *Capability) publishValidated(ctx context.Context, budget *capabilitydata.Budget, args []value.Value, kwargs map[string]value.Value) (value.Value, error) {
	method := c.PublishMethodName()
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{AllowRuntimeValues: true})
	payload, err := cloner.Hash(method+" payload", args[1])
	if err != nil {
		return value.NewNil(), err
	}
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := PublishRequest{Topic: args[0].String(), Payload: payload, Options: options}
	result, err := c.Publisher.Publish(ctx, req)
	if err != nil {
		return value.NewNil(), err
	}
	if ctx != nil {
		if err := ctx.Err(); err != nil {
			return value.NewNil(), err
		}
	}
	if err := budget.Refresh(); err != nil {
		return value.NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(method+" return value", result)
}

// nameArg coerces a string or symbol argument into its underlying name,
// rejecting empty values and other kinds.
func nameArg(method, label string, val value.Value) (string, error) {
	switch val.Kind() {
	case value.KindString, value.KindSymbol:
		name := val.String()
		if strings.TrimSpace(name) == "" {
			return "", fmt.Errorf("%s expects %s as non-empty string or symbol", method, label)
		}
		return name, nil
	default:
		return "", fmt.Errorf("%s expects %s as string or symbol", method, label)
	}
}

// isNilImpl reports whether impl is either an untyped nil or a typed-nil
// pointer/interface/etc. value.
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
