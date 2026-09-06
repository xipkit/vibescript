// Package contextcap provides a data-only capability adapter that resolves
// call-scoped context values into a script-visible hash or object.
//
// Resolvers receive the request context.Context and return a value.Value of
// kind Hash or Object. Returned values are validated to be data-only (no
// callables, no cycles) and are deep-cloned before being handed to the
// script runtime so subsequent mutations cannot leak across calls.
package contextcap

import (
	"context"
	"fmt"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/vibes/value"
)

// Resolver resolves call-scoped context data for script access.
type Resolver func(ctx context.Context) (value.Value, error)

// Capability is the data-only context capability implementation. Its Bind
// method takes a context.Context so the package stays free of vibes imports;
// the vibes facade wraps it to satisfy vibes.CapabilityAdapter.
type Capability struct {
	name     string
	resolver Resolver
}

// NewCapability constructs a data-only context capability.
func NewCapability(name string, resolver Resolver) (*Capability, error) {
	if name == "" {
		return nil, fmt.Errorf("vibes: context capability name must be non-empty")
	}
	if resolver == nil {
		return nil, fmt.Errorf("vibes: context capability requires a resolver")
	}
	return &Capability{name: name, resolver: resolver}, nil
}

// MustNewCapability constructs a capability or panics on invalid arguments.
func MustNewCapability(name string, resolver Resolver) *Capability {
	cap, err := NewCapability(name, resolver)
	if err != nil {
		panic(err)
	}
	return cap
}

// Name returns the script-visible binding name.
func (c *Capability) Name() string { return c.name }

// Bind resolves the underlying value, validates that it is data-only and
// non-cyclic, and returns a deep-cloned copy keyed by the capability name.
func (c *Capability) Bind(ctx context.Context) (map[string]value.Value, error) {
	val, err := c.resolver(ctx)
	if err != nil {
		return nil, fmt.Errorf("resolve %s capability: %w", c.name, err)
	}
	if ctx != nil {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
	}
	if val.Kind() != value.KindHash && val.Kind() != value.KindObject {
		return nil, fmt.Errorf("%s capability resolver must return hash/object", c.name)
	}
	label := c.name + " capability value"
	budget := capabilitydata.NewBudget(ctx, nil, nil)
	cloned, err := capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(label, val)
	if err != nil {
		return nil, err
	}
	return map[string]value.Value{c.name: cloned}, nil
}
