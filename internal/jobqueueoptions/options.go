// Package jobqueueoptions shares option parsing between public and runtime adapters.
package jobqueueoptions

import (
	"fmt"
	"time"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/vibes/value"
)

// Options is the internal representation of parsed enqueue options.
type Options struct {
	Delay  *time.Duration
	Key    *string
	Kwargs map[string]value.Value
}

// Parse retains delay/key checks and clones extra options with the request's memo.
// The validated mode permits runtime values already checked by the caller.
func Parse(name string, kwargs map[string]value.Value, budget *capabilitydata.Budget, cloner *capabilitydata.Cloner, validate bool) (Options, error) {
	if len(kwargs) == 0 {
		return Options{}, nil
	}

	if err := budget.ReserveMap(len(kwargs)); err != nil {
		return Options{}, err
	}
	validator := capabilitydata.NewValidator(budget)
	var delay *time.Duration
	var key *string
	extra := make(map[string]value.Value)

	for k, v := range kwargs {
		if err := budget.Work(len(k) + 1); err != nil {
			return Options{}, err
		}
		switch k {
		case "delay":
			d, err := valueToTimeDuration(name, v)
			if err != nil {
				return Options{}, err
			}
			if d < 0 {
				return Options{}, fmt.Errorf("%s.enqueue delay must be non-negative", name)
			}
			delay = &d
		case "key":
			if v.Kind() != value.KindString {
				return Options{}, fmt.Errorf("%s.enqueue key must be a string", name)
			}
			s := v.String()
			if s == "" {
				return Options{}, fmt.Errorf("%s.enqueue key must be non-empty", name)
			}
			key = &s
		default:
			if validate {
				label := fmt.Sprintf("%s.enqueue keyword %s", name, k)
				if err := validator.Validate(label, v); err != nil {
					return Options{}, err
				}
			}
			cloned, err := cloner.CloneWithOptions(name+".enqueue keyword "+k, v, capabilitydata.Options{AllowRuntimeValues: !validate})
			if err != nil {
				return Options{}, err
			}
			extra[k] = cloned
		}
	}

	opts := Options{Delay: delay, Key: key}
	if len(extra) > 0 {
		opts.Kwargs = extra
	}
	return opts, nil
}

func valueToTimeDuration(name string, val value.Value) (time.Duration, error) {
	switch val.Kind() {
	case value.KindDuration:
		secs := val.Duration().Seconds()
		return time.Duration(secs) * time.Second, nil
	case value.KindInt, value.KindFloat:
		secs, err := value.ValueToInt64(val)
		if err != nil {
			return 0, err
		}
		return time.Duration(secs) * time.Second, nil
	default:
		return 0, fmt.Errorf("%s.enqueue delay must be duration or numeric seconds", name)
	}
}
