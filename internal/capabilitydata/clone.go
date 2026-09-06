// Package capabilitydata bounds and isolates data graphs crossing host capabilities.
package capabilitydata

import (
	"context"
	"errors"
	"fmt"
	"math/bits"
	"reflect"
	"unsafe"

	"github.com/mgomes/vibescript/vibes/value"
)

// MaxDepth is the existing capability payload nesting limit.
const MaxDepth = 256

const (
	maxNodes       = 1 << 18
	maxEdges       = 1 << 20
	maxBytes       = 64 << 20
	maxWork        = 64 << 20
	valueBytes     = int(unsafe.Sizeof(value.Value{}))
	inlineMemoSize = 8
)

var (
	errCallable = errors.New("must be data-only")
	errCycle    = errors.New("must not contain cyclic references")
)

type limitError struct {
	message string
	cause   error
}

func (e *limitError) Error() string    { return e.message }
func (e *limitError) LimitError() bool { return true }
func (e *limitError) Unwrap() error    { return e.cause }

func labeledError(label string, err error) error {
	var limit interface{ LimitError() bool }
	if errors.As(err, &limit) && limit.LimitError() {
		return &limitError{message: label + " " + err.Error(), cause: err}
	}
	return fmt.Errorf("%s %w", label, err)
}

func exceeded(resource string, limit int) error {
	return &limitError{message: fmt.Sprintf("exceeds capability clone %s limit %d", resource, limit)}
}

// Budget accounts for cumulative work and allocations across independent snapshots
// in one operation. Callers must use it sequentially and release their external
// reservation after the operation. Nil hooks retain the standalone hard limits.
type Budget struct {
	ctx         context.Context
	chargeSteps func(int) error
	reserve     func(int) error
	nodes       int
	edges       int
	bytes       int
	work        int
	remainder   int
}

// NewBudget creates a bounded operation with optional interpreter step and memory
// hooks. Each charged step represents 64 units of graph or byte work.
func NewBudget(ctx context.Context, chargeSteps, reserve func(int) error) *Budget {
	return &Budget{ctx: ctx, chargeSteps: chargeSteps, reserve: reserve}
}

// Work charges work before a traversal, hash, copy, or allocation performs it.
func (b *Budget) Work(units int) error {
	if b.ctx != nil {
		if err := b.ctx.Err(); err != nil {
			return err
		}
	}
	if units < 0 || units > maxWork-b.work {
		return exceeded("work", maxWork)
	}
	b.work += units
	units += b.remainder
	b.remainder = units % 64
	if b.chargeSteps != nil && units >= 64 {
		return b.chargeSteps(units / 64)
	}
	return nil
}

// Reserve checks allocation size before it is passed to make or a constructor.
func (b *Budget) Reserve(bytes int) error {
	if bytes < 0 || bytes > maxBytes-b.bytes {
		return exceeded("byte", maxBytes)
	}
	if err := b.Work(bytes); err != nil {
		return err
	}
	if b.reserve != nil {
		if err := b.reserve(bytes); err != nil {
			return err
		}
	}
	b.bytes += bytes
	return nil
}

func (b *Budget) reserveSlots(count, size, base int) error {
	if count < 0 || base > maxBytes-b.bytes || count > (maxBytes-b.bytes-base)/size {
		return exceeded("byte", maxBytes)
	}
	return b.Reserve(base + count*size)
}

func (b *Budget) visit() error {
	if b.edges == maxEdges {
		return exceeded("edge", maxEdges)
	}
	b.edges++
	return b.Work(1)
}

func (b *Budget) checkEdges(count int) error {
	if count < 0 || count > maxEdges-b.edges {
		return exceeded("edge", maxEdges)
	}
	return nil
}

func (b *Budget) node() error {
	if b.nodes == maxNodes {
		return exceeded("node", maxNodes)
	}
	b.nodes++
	return b.Work(1)
}

// Options preserves the distinct contracts of validated option parsers and
// runtime containment copies. The default rejects runtime values and strips tags.
type Options struct {
	AllowRuntimeValues bool
	PreserveObjectTags bool
}

type nodeKey struct {
	id   uintptr
	kind value.ValueKind
	tag  value.ObjectTag
}

type cloneEntry struct {
	// Keep the source alive while its uintptr identity is in the memo.
	source value.Value
	cloned value.Value
	err    error
	height int
	active bool
}

type memoEntry struct {
	key nodeKey
	cloneEntry
}

// Cloner preserves shared children across every root of one immutable input
// graph. Create a new Cloner after invoking host code or yielding a snapshot;
// reuse its Budget to retain cumulative limits without retaining stale clones.
type Cloner struct {
	budget      *Budget
	options     Options
	memo        map[nodeKey]cloneEntry
	inline      [inlineMemoSize]memoEntry
	inlineCount int
	err         error
}

// NewCloner starts an independent identity memo using the operation's budget.
func NewCloner(budget *Budget, options Options) *Cloner {
	if budget == nil {
		budget = NewBudget(context.Background(), nil, nil)
	}
	return &Cloner{budget: budget, options: options}
}

// Clone validates and isolates one root, preserving aliases to earlier roots.
func (c *Cloner) Clone(label string, source value.Value) (value.Value, error) {
	if c.err != nil {
		return value.NewNil(), c.err
	}
	cloned, _, err := c.clone(source, 0)
	if err != nil {
		c.err = labeledError(label, err)
		return value.NewNil(), c.err
	}
	return cloned, nil
}

// Kwargs isolates keyword values with the same memo as positional roots.
func (c *Cloner) Kwargs(method string, source map[string]value.Value) (map[string]value.Value, error) {
	if c.err != nil {
		return nil, c.err
	}
	if len(source) == 0 {
		return nil, nil
	}
	if err := c.budget.checkEdges(len(source)); err != nil {
		return nil, labeledError(method+" keywords", err)
	}
	if err := c.reserveMap(len(source)); err != nil {
		return nil, labeledError(method+" keywords", err)
	}
	out := make(map[string]value.Value, len(source))
	for key, item := range source {
		if err := c.budget.Work(len(key)); err != nil {
			return nil, labeledError(method+" keywords", err)
		}
		cloned, err := c.Clone(method+" keyword "+key, item)
		if err != nil {
			return nil, err
		}
		out[key] = cloned
	}
	return out, nil
}

func (c *Cloner) clone(source value.Value, depth int) (value.Value, int, error) {
	if depth > MaxDepth {
		return value.NewNil(), 0, &limitError{message: fmt.Sprintf("exceeds maximum depth %d", MaxDepth)}
	}
	if err := c.budget.visit(); err != nil {
		return value.NewNil(), 0, err
	}
	switch source.Kind() {
	case value.KindFunction, value.KindBuiltin, value.KindBlock, value.KindClass, value.KindInstance, value.KindShape:
		if !c.options.AllowRuntimeValues {
			return value.NewNil(), 0, errCallable
		}
		return source, 0, nil
	case value.KindArray, value.KindHash, value.KindObject:
	default:
		return source, 0, nil
	}
	key := c.identity(source)
	if entry, ok := c.lookup(key); ok {
		if entry.active {
			return value.NewNil(), 0, errCycle
		}
		if entry.height > MaxDepth-depth {
			return value.NewNil(), 0, &limitError{message: fmt.Sprintf("exceeds maximum depth %d", MaxDepth)}
		}
		return entry.cloned, entry.height, entry.err
	}
	if err := c.budget.node(); err != nil {
		return value.NewNil(), 0, err
	}
	if err := c.remember(key, cloneEntry{source: source, active: true}); err != nil {
		return value.NewNil(), 0, err
	}
	var cloned value.Value
	var height int
	var err error
	if source.Kind() == value.KindArray {
		cloned, height, err = c.cloneArray(source, depth)
	} else {
		cloned, height, err = c.cloneMap(source, depth)
	}
	c.complete(key, cloneEntry{source: source, cloned: cloned, height: height, err: err})
	return cloned, height, err
}

func (c *Cloner) lookup(key nodeKey) (cloneEntry, bool) {
	if c.memo != nil {
		entry, ok := c.memo[key]
		return entry, ok
	}
	for i := range c.inlineCount {
		if c.inline[i].key == key {
			return c.inline[i].cloneEntry, true
		}
	}
	return cloneEntry{}, false
}

func (c *Cloner) remember(key nodeKey, entry cloneEntry) error {
	const slotBytes = 2 * (int(unsafe.Sizeof(memoEntry{})) + 32)
	if c.memo == nil && c.inlineCount < len(c.inline) {
		c.inline[c.inlineCount] = memoEntry{key: key, cloneEntry: entry}
		c.inlineCount++
		return nil
	}
	if c.memo == nil {
		if err := c.budget.reserveSlots(c.inlineCount+1, slotBytes, 64); err != nil {
			return err
		}
		c.memo = make(map[nodeKey]cloneEntry, c.inlineCount+1)
		for i := range c.inlineCount {
			c.memo[c.inline[i].key] = c.inline[i].cloneEntry
		}
		clear(c.inline[:])
	} else if err := c.budget.Reserve(slotBytes); err != nil {
		return err
	}
	c.memo[key] = entry
	return nil
}

func (c *Cloner) complete(key nodeKey, entry cloneEntry) {
	if c.memo != nil {
		c.memo[key] = entry
		return
	}
	for i := range c.inlineCount {
		if c.inline[i].key == key {
			c.inline[i].cloneEntry = entry
			return
		}
	}
}

func (c *Cloner) identity(source value.Value) nodeKey {
	key := nodeKey{kind: source.Kind()}
	switch source.Kind() {
	case value.KindArray:
		key.id = value.ArrayIdentity(source)
	case value.KindHash:
		key.id = value.HashIdentity(source)
	case value.KindObject:
		key.id = reflect.ValueOf(source.HashEntryMap()).Pointer()
		if c.options.PreserveObjectTags {
			key.tag = source.ObjectTag()
		}
	}
	return key
}

func (c *Cloner) cloneArray(source value.Value, depth int) (value.Value, int, error) {
	items := source.Array()
	if err := c.budget.checkEdges(len(items)); err != nil {
		return value.NewNil(), 0, err
	}
	if err := c.budget.reserveSlots(len(items), valueBytes, value.ArrayDataBytes); err != nil {
		return value.NewNil(), 0, err
	}
	out := make([]value.Value, len(items))
	height := 0
	var cycle error
	for i, item := range items {
		cloned, childHeight, err := c.clone(item, depth+1)
		if err != nil && !errors.Is(err, errCycle) {
			return value.NewNil(), 0, err
		}
		if errors.Is(err, errCycle) {
			cycle = err
		}
		height = max(height, childHeight+1)
		out[i] = cloned
	}
	if cycle != nil {
		return value.NewNil(), height, cycle
	}
	return value.NewArray(out), height, nil
}

func (c *Cloner) reserveMap(count int) error {
	// Include capacity slack and a minimum group, as in the runtime's
	// structural map estimates, before any bucket or key-order allocation.
	const slotBytes = 2 * (16 + valueBytes + 32)
	return c.budget.reserveSlots(count, slotBytes, 64+8*slotBytes)
}

func (c *Cloner) cloneMap(source value.Value, depth int) (value.Value, int, error) {
	items := source.HashEntryMap()
	if err := c.budget.checkEdges(len(items)); err != nil {
		return value.NewNil(), 0, err
	}
	if err := c.reserveMap(len(items)); err != nil {
		return value.NewNil(), 0, err
	}
	// A key-order fallback sorts the map keys. Account for comparison and
	// rehashing work, including long common prefixes, before that helper runs.
	factor := 1
	if source.Kind() == value.KindHash {
		factor += bits.Len(uint(len(items)))
	}
	scalarOnly := true
	for key, item := range items {
		if len(key) > (maxWork-c.budget.work)/factor {
			return value.NewNil(), 0, exceeded("work", maxWork)
		}
		if err := c.budget.Work(len(key)*factor + 1); err != nil {
			return value.NewNil(), 0, err
		}
		if !scalar(item.Kind()) {
			scalarOnly = false
		}
	}
	out := make(map[string]value.Value, len(items))
	height := 0
	var cycle error
	for key, item := range items {
		childDepth := depth + 1
		// Preserve the existing scalar-map fast path's depth contract.
		if scalarOnly {
			childDepth = depth
		}
		cloned, childHeight, err := c.clone(item, childDepth)
		if err != nil && !errors.Is(err, errCycle) {
			return value.NewNil(), 0, err
		}
		if errors.Is(err, errCycle) {
			cycle = err
		}
		if !scalarOnly {
			height = max(height, childHeight+1)
		}
		out[key] = cloned
	}
	if cycle != nil {
		return value.NewNil(), height, cycle
	}
	if source.Kind() == value.KindHash {
		if err := c.budget.reserveSlots(len(items), valueBytes+16, value.HashDataBytes); err != nil {
			return value.NewNil(), 0, err
		}
		return value.NewHashWithTrustedOrder(out, source.HashKeyOrder()), height, nil
	}
	if err := c.budget.Reserve(value.ObjectDataBytes); err != nil {
		return value.NewNil(), 0, err
	}
	if c.options.PreserveObjectTags {
		if text, ok := source.ObjectStringForm(); ok {
			return value.NewTaggedObject(out, source.ObjectTag(), text), height, nil
		}
	}
	return value.NewObject(out), height, nil
}

func scalar(kind value.ValueKind) bool {
	switch kind {
	case value.KindNil, value.KindBool, value.KindInt, value.KindFloat, value.KindString,
		value.KindMoney, value.KindDuration, value.KindTime, value.KindSymbol, value.KindRange, value.KindRegex:
		return true
	default:
		return false
	}
}
