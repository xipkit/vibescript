package db

import (
	"fmt"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/vibes/value"
)

// CallFind implements the db.find boundary: arg validation, host
// invocation, and cloning of the host-returned Value so script-side
// state cannot alias the host's data.
func (c *Capability) CallFind(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	if err := c.validateFindCallShapeArgs(args, kwargs, block); err != nil {
		return value.NewNil(), err
	}
	return c.callFindValidated(exec, args, kwargs, block)
}

func (c *Capability) callFindValidated(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	method := c.name + ".find"
	budget := capabilitydata.ExecutionBudget(exec)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	id, err := cloner.Clone(method+" id", args[1])
	if err != nil {
		return value.NewNil(), err
	}
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := DBFindRequest{
		Collection: args[0].String(),
		ID:         id,
		Options:    options,
	}
	result, err := c.db.Find(exec.Context(), req)
	if err != nil {
		return value.NewNil(), err
	}
	if err := contextErr(exec.Context()); err != nil {
		return value.NewNil(), err
	}
	if err := budget.Refresh(); err != nil {
		return value.NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(method+" return value", result)
}

// CallQuery implements the db.query boundary.
func (c *Capability) CallQuery(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	if err := c.validateQueryCallShapeArgs(args, kwargs, block); err != nil {
		return value.NewNil(), err
	}
	return c.callQueryValidated(exec, args, kwargs, block)
}

func (c *Capability) callQueryValidated(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	method := c.name + ".query"
	budget := capabilitydata.ExecutionBudget(exec)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := DBQueryRequest{
		Collection: args[0].String(),
		Options:    options,
	}
	result, err := c.db.Query(exec.Context(), req)
	if err != nil {
		return value.NewNil(), err
	}
	if err := contextErr(exec.Context()); err != nil {
		return value.NewNil(), err
	}
	if err := budget.Refresh(); err != nil {
		return value.NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(method+" return value", result)
}

// CallUpdate implements the db.update boundary.
func (c *Capability) CallUpdate(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	if err := c.validateUpdateCallShapeArgs(args, kwargs, block); err != nil {
		return value.NewNil(), err
	}
	return c.callUpdateValidated(exec, args, kwargs, block)
}

func (c *Capability) callUpdateValidated(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	method := c.name + ".update"
	budget := capabilitydata.ExecutionBudget(exec)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	id, err := cloner.Clone(method+" id", args[1])
	if err != nil {
		return value.NewNil(), err
	}
	attributes, err := cloner.Hash(method+" attributes", args[2])
	if err != nil {
		return value.NewNil(), err
	}
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := DBUpdateRequest{
		Collection: args[0].String(),
		ID:         id,
		Attributes: attributes,
		Options:    options,
	}
	result, err := c.db.Update(exec.Context(), req)
	if err != nil {
		return value.NewNil(), err
	}
	if err := contextErr(exec.Context()); err != nil {
		return value.NewNil(), err
	}
	if err := budget.Refresh(); err != nil {
		return value.NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(method+" return value", result)
}

// CallSum implements the db.sum boundary.
func (c *Capability) CallSum(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	if err := c.validateSumCallShapeArgs(args, kwargs, block); err != nil {
		return value.NewNil(), err
	}
	return c.callSumValidated(exec, args, kwargs, block)
}

func (c *Capability) callSumValidated(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	method := c.name + ".sum"
	budget := capabilitydata.ExecutionBudget(exec)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := DBSumRequest{
		Collection: args[0].String(),
		Field:      args[1].String(),
		Options:    options,
	}
	result, err := c.db.Sum(exec.Context(), req)
	if err != nil {
		return value.NewNil(), err
	}
	if err := contextErr(exec.Context()); err != nil {
		return value.NewNil(), err
	}
	if err := budget.Refresh(); err != nil {
		return value.NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(method+" return value", result)
}

// CallEach implements the db.each boundary. The host returns the row
// set up front; the capability charges each row and its bounded data-only
// copy, then yields the independent snapshot to
// the script-supplied block.
func (c *Capability) CallEach(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	if err := c.validateEachCallShapeArgs(args, kwargs, block); err != nil {
		return value.NewNil(), err
	}
	return c.callEachValidated(exec, args, kwargs, block)
}

func (c *Capability) callEachValidated(exec ExecutionContext, args []value.Value, kwargs map[string]value.Value, block value.Value) (value.Value, error) {
	method := c.name + ".each"
	budget := capabilitydata.ExecutionBudget(exec)
	cloner := capabilitydata.NewCloner(budget, capabilitydata.Options{})
	options, err := cloner.Kwargs(method, kwargs)
	if err != nil {
		return value.NewNil(), err
	}
	req := DBEachRequest{
		Collection: args[0].String(),
		Options:    options,
	}
	rows, err := c.db.Each(exec.Context(), req)
	if err != nil {
		return value.NewNil(), err
	}
	if ctx := exec.Context(); ctx != nil {
		if err := ctx.Err(); err != nil {
			return value.NewNil(), err
		}
	}
	for idx, row := range rows {
		if err := budget.Refresh(); err != nil {
			return value.NewNil(), err
		}
		if err := exec.Step(); err != nil {
			return value.NewNil(), err
		}
		cloned, err := capabilitydata.NewCloner(budget, capabilitydata.Options{}).Clone(fmt.Sprintf("%s row %d", method, idx), row)
		if err != nil {
			return value.NewNil(), err
		}
		if _, err := exec.CallBlock(block, []value.Value{cloned}); err != nil {
			return value.NewNil(), err
		}
		if ctx := exec.Context(); ctx != nil {
			if err := ctx.Err(); err != nil {
				return value.NewNil(), err
			}
		}
	}
	return value.NewNil(), nil
}

func contextErr(ctx interface{ Err() error }) error {
	if ctx == nil {
		return nil
	}
	return ctx.Err()
}
