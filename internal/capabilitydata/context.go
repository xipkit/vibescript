package capabilitydata

import "context"

type budgetContext struct {
	context.Context
	budget *Budget
}

// WithBudget transports a budget only between runtime and first-party adapters.
// The receiver must unpack it before passing the context to host code.
func WithBudget(ctx context.Context, budget *Budget) context.Context {
	return budgetContext{Context: ctx, budget: budget}
}

// UnpackBudget returns the exact original context and operation budget. Ordinary
// callers receive a standalone budget; no state is stored on their context.
func UnpackBudget(ctx context.Context) (context.Context, *Budget) {
	if wrapped, ok := ctx.(budgetContext); ok {
		return wrapped.Context, wrapped.budget
	}
	return ctx, NewBudget(ctx, nil, nil)
}

// Execution is the optional host execution context needed for budget fallback.
type Execution interface {
	Context() context.Context
	Step() error
}

// ExecutionBudget uses the runtime's private provider when available, preserving
// the existing public execution interfaces for direct embedders.
func ExecutionBudget(exec Execution) *Budget {
	if provider, ok := exec.(interface{ CapabilityDataBudget() *Budget }); ok {
		return provider.CapabilityDataBudget()
	}
	return NewBudget(exec.Context(), func(steps int) error {
		for range steps {
			if err := exec.Step(); err != nil {
				return err
			}
		}
		return nil
	}, nil)
}
