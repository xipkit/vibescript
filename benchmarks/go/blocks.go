package main

import (
	"fmt"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type blockCapability struct{}

// Bind exposes synchronous block drivers for shared conformance cases.
func (blockCapability) Bind(_ vibes.CapabilityBinding) (map[string]value.Value, error) {
	once := vibes.NewBuiltin("blocks.once", func(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		return exec.CallBlock(block, args)
	})
	each := vibes.NewBuiltin("blocks.each", func(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		var result []value.Value
		for _, item := range args[0].Array() {
			item, err := exec.CallBlock(block, []value.Value{item})
			if err != nil {
				return value.NewNil(), err
			}
			result = append(result, item)
		}
		return value.NewArray(result), nil
	})
	optional := vibes.NewBuiltin("blocks.optional", func(_ *vibes.Execution, _ value.Value, _ []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		return value.NewBool(!block.IsNil()), nil
	})
	recover := vibes.NewBuiltin("blocks.recover", func(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		result, err := exec.CallBlock(block, args)
		if err != nil {
			return value.NewString("recovered"), nil
		}
		return result, nil
	})
	ignore := vibes.NewBuiltin("blocks.ignore", func(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		if _, err := exec.CallBlock(block, args); err != nil {
			// Deliberately ignore both errors to characterize control preservation.
			_, _ = exec.CallBlock(block, args)
		}
		return value.NewInt(99), nil
	})
	checked := vibes.NewBuiltin("blocks.checked", func(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
		return exec.CallBlock(block, args)
	})
	keywords := vibes.NewBuiltin("blocks.keywords", func(exec *vibes.Execution, _ value.Value, args []value.Value, keywords map[string]value.Value, block value.Value) (value.Value, error) {
		return exec.CallBlock(block, []value.Value{value.NewArray(args), value.NewObject(keywords)})
	})
	return map[string]value.Value{"blocks": value.NewObject(map[string]value.Value{
		"once": once, "each": each, "optional": optional, "recover": recover,
		"ignore": ignore, "checked": checked, "keywords": keywords,
	})}, nil
}

// CapabilityContracts checks block presence and absorbed break values.
func (blockCapability) CapabilityContracts() map[string]vibes.CapabilityMethodContract {
	return map[string]vibes.CapabilityMethodContract{"blocks.checked": {
		ValidateArgs: func(args []value.Value, keywords map[string]value.Value, block value.Value) error {
			if len(args) != 1 || len(keywords) != 0 || block.IsNil() {
				return fmt.Errorf("one argument and block required")
			}
			return nil
		},
		ValidateReturn: func(result value.Value) error {
			if result.Kind() != value.KindInt {
				return fmt.Errorf("integer result required")
			}
			return nil
		},
	}}
}
