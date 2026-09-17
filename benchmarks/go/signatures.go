package main

import (
	"fmt"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type signatureProbe struct {
	Params       []vibes.SignatureParam `json:"params"`
	Result       string                 `json:"result"`
	AcceptsBlock bool                   `json:"accepts_block"`
	Callback     string                 `json:"callback"`
	Registration string                 `json:"registration"`
	Contract     bool                   `json:"contract"`
}

func (p signatureProbe) signature() vibes.Signature {
	return vibes.Signature{Params: p.Params, Result: p.Result, AcceptsBlock: p.AcceptsBlock}
}

func (p signatureProbe) call(exec *vibes.Execution, _ value.Value, args []value.Value, _ map[string]value.Value, block value.Value) (value.Value, error) {
	switch p.Callback {
	case "block":
		return exec.CallBlock(block, args)
	case "list":
		return value.NewArray(args), nil
	case "symbol":
		return value.NewSymbol("draft"), nil
	case "bad":
		return value.NewString("bad"), nil
	case "kind":
		return value.NewString(args[0].Kind().String()), nil
	default:
		return args[0], nil
	}
}

func (p signatureProbe) method() (value.Value, error) {
	name := "typed.echo"
	if p.Registration == "global" || p.Registration == "registered" {
		name = "echo"
	}
	return vibes.NewTypedBuiltin(name, p.call, p.signature())
}

// Bind grants the typed probe without detaching its method in script code.
func (p signatureProbe) Bind(_ vibes.CapabilityBinding) (map[string]value.Value, error) {
	method, err := p.method()
	if err != nil {
		return nil, err
	}
	return map[string]value.Value{"typed": value.NewObject(map[string]value.Value{"echo": method})}, nil
}

// CapabilityContracts verifies normalization order around an independent contract.
func (p signatureProbe) CapabilityContracts() map[string]vibes.CapabilityMethodContract {
	if !p.Contract {
		return nil
	}
	return map[string]vibes.CapabilityMethodContract{"typed.echo": {
		ValidateArgs: func(args []value.Value, _ map[string]value.Value, _ value.Value) error {
			if args[0].Kind() != value.KindSymbol {
				return fmt.Errorf("raw symbol required")
			}
			return nil
		},
		ValidateReturn: func(result value.Value) error {
			if result.Kind() != value.KindEnumValue {
				return fmt.Errorf("normalized enum required")
			}
			return nil
		},
	}}
}
