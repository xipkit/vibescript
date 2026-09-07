package runtime

import "github.com/mgomes/vibescript/vibes/value"

// The parser cannot call script code or publish its partial containers. One
// baseline therefore stays valid while used tracks all unfinished parents,
// pending keys, and child values, without walking growing prefixes.
func (p *jsonValueParser) reserve(n int) error {
	if err := p.checkExtra(n); err != nil {
		return err
	}
	p.used = saturatingAdd(p.used, n)
	return nil
}

func (p *jsonValueParser) checkExtra(n int) error {
	if p.exec != nil && p.exec.memoryExceeded(saturatingAdd(p.base, saturatingAdd(p.used, n))) {
		return p.exec.memoryQuotaExceededError()
	}
	return nil
}

// Only displaced JSON trees reach this walk. They cannot alias other parsed
// containers, so each allocation is visited at most once when it is discarded.
// Match the reservations above, including both retained key representations.
func (p *jsonValueParser) discardedPayload(v Value) (int, error) {
	if p.exec != nil {
		if err := p.exec.step(); err != nil {
			return 0, err
		}
	}
	size := 0
	switch v.Kind() {
	case KindString:
		return estimatedStringHeaderBytes + len(v.String()), nil
	case KindInt:
		if bi, ok := value.BigIntPayload(v); ok {
			return estimatedBigIntStructBytes + cap(bi.Bits())*estimatedBigIntWordBytes, nil
		}
	case KindArray:
		values := v.Array()
		size = nestedArrayBackingBytes(cap(values)) + len(values)*estimatedValueBytes
		for _, child := range values {
			n, err := p.discardedPayload(child)
			if err != nil {
				return 0, err
			}
			size += n
		}
	case KindHash:
		size = estimatedMapBaseBytes + estimatedHashDataBytes + value.HashEntryCapacity(v)*estimatedMapEntryStructuralBytes + hashOrderBackingBytes(value.HashOrderCapacity(v))
		for key, child := range v.HashEntryMap() {
			n, err := p.discardedPayload(child)
			if err != nil {
				return 0, err
			}
			size += estimatedStringHeaderBytes + 2*len(key) + n
		}
	}
	return size, nil
}
