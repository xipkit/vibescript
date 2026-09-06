package capabilitydata

import (
	"errors"

	"github.com/mgomes/vibescript/vibes/value"
)

// Validator checks a complete argument graph without copying its containers.
// Depth errors precede callable errors, which precede cycle errors.
type Validator struct {
	cloner *Cloner
}

// NewValidator starts a validation memo sharing the operation's budget.
func NewValidator(budget *Budget) *Validator {
	cloner := NewCloner(budget, Options{AllowRuntimeValues: true})
	cloner.validateOnly = true
	return &Validator{cloner: cloner}
}

// Validate rejects runtime values, cycles, and excessive graph depth or work.
func (v *Validator) Validate(label string, source value.Value) error {
	_, _, traits, err := v.cloner.clone(source, 0, v.cloner.options)
	if err != nil && !errors.Is(err, errCycle) {
		return labeledError(label, err)
	}
	if traits&hasRuntimeValues != 0 {
		return labeledError(label, errCallable)
	}
	if err != nil {
		return labeledError(label, err)
	}
	return nil
}

// Kwargs validates every keyword using one memo and cumulative budget.
func (v *Validator) Kwargs(method string, kwargs map[string]value.Value) error {
	if err := v.cloner.budget.checkEdges(len(kwargs)); err != nil {
		return labeledError(method+" keywords", err)
	}
	for key, item := range kwargs {
		if err := v.cloner.budget.Work(len(key)); err != nil {
			return labeledError(method+" keywords", err)
		}
		if err := v.Validate(method+" keyword "+key, item); err != nil {
			return err
		}
	}
	return nil
}

func (c *Cloner) validateMap(source value.Value, depth int, options Options) (value.Value, int, graphTraits, error) {
	items := source.HashEntryMap()
	if err := c.budget.checkEdges(len(items)); err != nil {
		return value.NewNil(), 0, 0, err
	}
	height := 0
	var traits graphTraits
	var cycle error
	if source.Kind() == value.KindObject && source.ObjectTag() != value.ObjectTagNone {
		traits |= hasTags
	}
	for _, item := range items {
		_, childHeight, childTraits, err := c.clone(item, depth+1, options)
		if err != nil && !errors.Is(err, errCycle) {
			return value.NewNil(), 0, 0, err
		}
		if errors.Is(err, errCycle) {
			cycle = err
		}
		height = max(height, childHeight+1)
		traits |= childTraits
	}
	return source, height, traits, cycle
}
