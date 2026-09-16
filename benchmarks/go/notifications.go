package main

import (
	"fmt"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

type notificationCapability struct {
	name string
}

// Bind supplies the website's deterministic notification preview.
func (capability notificationCapability) Bind(binding vibes.CapabilityBinding) (map[string]value.Value, error) {
	var names []string
	switch capability.name {
	case "sms":
		names = []string{"to", "body"}
	case "email":
		names = []string{"to", "subject", "body"}
	default:
		return nil, fmt.Errorf("unknown notification preview %q", capability.name)
	}
	params := make([]vibes.SignatureParam, len(names))
	for i, name := range names {
		params[i] = vibes.SignatureParam{Name: name, Type: "string"}
	}
	send, err := vibes.NewTypedBuiltin(capability.name+".send", func(
		_ *vibes.Execution,
		_ value.Value,
		args []value.Value,
		_ map[string]value.Value,
		_ value.Value,
	) (value.Value, error) {
		if err := binding.Context.Err(); err != nil {
			return value.NewNil(), err
		}
		result := map[string]value.Value{"status": value.NewString("preview")}
		for i, name := range names {
			result[name] = args[i]
		}
		return value.NewHash(result), nil
	}, vibes.Signature{Params: params, Result: "hash"})
	if err != nil {
		return nil, err
	}
	return map[string]value.Value{
		capability.name: value.NewObject(map[string]value.Value{"send": send}),
	}, nil
}
