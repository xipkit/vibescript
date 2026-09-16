package main

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"strconv"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

func encodeResult(ctx context.Context, converter *vibes.Script, result value.Value, encoding string) (string, error) {
	switch encoding {
	case "", "json":
		encoded, err := converter.Call(ctx, "encode", []value.Value{result}, vibes.CallOptions{})
		if err != nil {
			return "", err
		}
		return encoded.String(), nil
	case "typed":
		node, err := typedResult(result, 0)
		if err != nil {
			return "", err
		}
		encoded, err := json.Marshal([]any{"typed-v1", node})
		if err != nil {
			return "", err
		}
		return string(encoded), nil
	default:
		return "", fmt.Errorf("unknown result encoding %q", encoding)
	}
}

func typedResult(result value.Value, depth int) (any, error) {
	if depth > 256 {
		return nil, fmt.Errorf("typed result nesting exceeds 256")
	}
	kind := result.Kind().String()
	switch result.Kind() {
	case value.KindNil:
		return []any{kind}, nil
	case value.KindBool:
		return []any{kind, result.Bool()}, nil
	case value.KindInt:
		return []any{kind, result.BigInt().String()}, nil
	case value.KindFloat:
		return []any{kind, fmt.Sprintf("%016x", math.Float64bits(result.Float()))}, nil
	case value.KindString, value.KindSymbol:
		return []any{kind, hex.EncodeToString([]byte(result.String()))}, nil
	case value.KindMoney:
		money := result.Money()
		return []any{kind, money.Currency(), strconv.FormatInt(money.Cents(), 10)}, nil
	case value.KindDuration:
		return []any{kind, strconv.FormatInt(result.Duration().Seconds(), 10)}, nil
	case value.KindArray:
		items := make([]any, len(result.Array()))
		for i, item := range result.Array() {
			encoded, err := typedResult(item, depth+1)
			if err != nil {
				return nil, err
			}
			items[i] = encoded
		}
		return []any{kind, items}, nil
	case value.KindHash, value.KindObject:
		entries := result.HashEntries()
		items := make([]any, len(entries))
		for i, entry := range entries {
			encoded, err := typedResult(entry.Value, depth+1)
			if err != nil {
				return nil, err
			}
			items[i] = []any{hex.EncodeToString([]byte(entry.Key.String())), encoded}
		}
		return []any{kind, items}, nil
	default:
		return nil, fmt.Errorf("unsupported typed result %s", kind)
	}
}
