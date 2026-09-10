package runtime

import (
	"fmt"
	"math/big"
	"reflect"
	"strings"
	"sync/atomic"
	"time"
	"unicode/utf8"
	"weak"

	"github.com/mgomes/vibescript/internal/ast"
	"github.com/mgomes/vibescript/vibes/source"
	"github.com/mgomes/vibescript/vibes/value"
)

// Position is an internal alias for source.Position so runtime code can
// use the short name. AST and other internal aliases below mirror the
// vibes facade re-exports.
type Position = source.Position

type (
	Node       = ast.Node
	Statement  = ast.Statement
	Expression = ast.Expression
	Program    = ast.Program

	Param     = ast.Param
	ParamKind = ast.ParamKind
	TypeExpr  = ast.TypeExpr
	TypeKind  = ast.TypeKind

	Token     = ast.Token
	TokenType = ast.TokenType

	FunctionStmt   = ast.FunctionStmt
	ReturnStmt     = ast.ReturnStmt
	RaiseStmt      = ast.RaiseStmt
	AliasStmt      = ast.AliasStmt
	AssignStmt     = ast.AssignStmt
	ExprStmt       = ast.ExprStmt
	IfStmt         = ast.IfStmt
	ForStmt        = ast.ForStmt
	WhileStmt      = ast.WhileStmt
	UntilStmt      = ast.UntilStmt
	BreakStmt      = ast.BreakStmt
	NextStmt       = ast.NextStmt
	RetryStmt      = ast.RetryStmt
	RescueClause   = ast.RescueClause
	TryStmt        = ast.TryStmt
	PropertyDecl   = ast.PropertyDecl
	ClassStmt      = ast.ClassStmt
	EnumMemberStmt = ast.EnumMemberStmt
	EnumStmt       = ast.EnumStmt

	Identifier         = ast.Identifier
	IntegerLiteral     = ast.IntegerLiteral
	FloatLiteral       = ast.FloatLiteral
	StringLiteral      = ast.StringLiteral
	RegexLiteral       = ast.RegexLiteral
	BoolLiteral        = ast.BoolLiteral
	NilLiteral         = ast.NilLiteral
	SymbolLiteral      = ast.SymbolLiteral
	ArrayLiteral       = ast.ArrayLiteral
	HashPair           = ast.HashPair
	HashLiteral        = ast.HashLiteral
	CallExpr           = ast.CallExpr
	KeywordArg         = ast.KeywordArg
	SplatArg           = ast.SplatArg
	TypeLiteral        = ast.TypeLiteral
	MemberExpr         = ast.MemberExpr
	ScopeExpr          = ast.ScopeExpr
	IndexExpr          = ast.IndexExpr
	DestructureElement = ast.DestructureElement
	DestructureTarget  = ast.DestructureTarget
	IvarExpr           = ast.IvarExpr
	ClassVarExpr       = ast.ClassVarExpr
	UnaryExpr          = ast.UnaryExpr
	BinaryExpr         = ast.BinaryExpr
	ConditionalExpr    = ast.ConditionalExpr
	RescueExpr         = ast.RescueExpr
	IfExprBranch       = ast.IfExprBranch
	IfExpr             = ast.IfExpr
	RangeExpr          = ast.RangeExpr
	CaseWhenClause     = ast.CaseWhenClause
	CaseExpr           = ast.CaseExpr
	BlockLiteral       = ast.BlockLiteral
	YieldExpr          = ast.YieldExpr
	InterpolatedString = ast.InterpolatedString
	InterpolatedSymbol = ast.InterpolatedSymbol
	StringPart         = ast.StringPart
	StringText         = ast.StringText
	StringExpr         = ast.StringExpr
)

const (
	ParamNormal      = ast.ParamNormal
	ParamKeyword     = ast.ParamKeyword
	ParamRest        = ast.ParamRest
	ParamKeywordRest = ast.ParamKeywordRest
)

const (
	TypeAny      = ast.TypeAny
	TypeInt      = ast.TypeInt
	TypeFloat    = ast.TypeFloat
	TypeNumber   = ast.TypeNumber
	TypeString   = ast.TypeString
	TypeBool     = ast.TypeBool
	TypeNil      = ast.TypeNil
	TypeDuration = ast.TypeDuration
	TypeTime     = ast.TypeTime
	TypeMoney    = ast.TypeMoney
	TypeArray    = ast.TypeArray
	TypeHash     = ast.TypeHash
	TypeRange    = ast.TypeRange
	TypeSymbol   = ast.TypeSymbol
	TypeFunction = ast.TypeFunction
	TypeShape    = ast.TypeShape
	TypeUnion    = ast.TypeUnion
	TypeEnum     = ast.TypeEnum
	TypeUnknown  = ast.TypeUnknown
)

const (
	tokenIllegal   = ast.TokenIllegal
	tokenEOF       = ast.TokenEOF
	tokenIdent     = ast.TokenIdent
	tokenInt       = ast.TokenInt
	tokenFloat     = ast.TokenFloat
	tokenString    = ast.TokenString
	tokenSymbol    = ast.TokenSymbol
	tokenAssign    = ast.TokenAssign
	tokenNone      = ast.TokenNone
	tokenPlus      = ast.TokenPlus
	tokenMinus     = ast.TokenMinus
	tokenBang      = ast.TokenBang
	tokenAsterisk  = ast.TokenAsterisk
	tokenPower     = ast.TokenPower
	tokenSlash     = ast.TokenSlash
	tokenPercent   = ast.TokenPercent
	tokenAndAssign = ast.TokenAndAssign
	tokenOrAssign  = ast.TokenOrAssign
	tokenLT        = ast.TokenLT
	tokenShovel    = ast.TokenShovel
	tokenGT        = ast.TokenGT
	tokenLTE       = ast.TokenLTE
	tokenGTE       = ast.TokenGTE
	tokenSpaceship = ast.TokenSpaceship
	tokenEQ        = ast.TokenEQ
	tokenCaseEQ    = ast.TokenCaseEQ
	tokenMatch     = ast.TokenMatch
	tokenNotMatch  = ast.TokenNotMatch
	tokenNotEQ     = ast.TokenNotEQ
	tokenAnd       = ast.TokenAnd
	tokenOr        = ast.TokenOr
	tokenAmpersand = ast.TokenAmpersand
	tokenQuestion  = ast.TokenQuestion
	tokenComma     = ast.TokenComma
	tokenColon     = ast.TokenColon
	tokenScope     = ast.TokenScope
	tokenDot       = ast.TokenDot
	tokenRange     = ast.TokenRange
	tokenLParen    = ast.TokenLParen
	tokenRParen    = ast.TokenRParen
	tokenLBrace    = ast.TokenLBrace
	tokenRBrace    = ast.TokenRBrace
	tokenLBracket  = ast.TokenLBracket
	tokenRBracket  = ast.TokenRBracket
	tokenPipe      = ast.TokenPipe
	tokenArrow     = ast.TokenArrow
	tokenThinArrow = ast.TokenThinArrow
	tokenIvar      = ast.TokenIvar
	tokenClassVar  = ast.TokenClassVar
	tokenDef       = ast.TokenDef
	tokenClass     = ast.TokenClass
	tokenEnum      = ast.TokenEnum
	tokenExport    = ast.TokenExport
	tokenSelf      = ast.TokenSelf
	tokenPrivate   = ast.TokenPrivate
	tokenProperty  = ast.TokenProperty
	tokenGetter    = ast.TokenGetter
	tokenSetter    = ast.TokenSetter
	tokenBegin     = ast.TokenBegin
	tokenRescue    = ast.TokenRescue
	tokenEnsure    = ast.TokenEnsure
	tokenRaise     = ast.TokenRaise
	tokenEnd       = ast.TokenEnd
	tokenReturn    = ast.TokenReturn
	tokenYield     = ast.TokenYield
	tokenDo        = ast.TokenDo
	tokenFor       = ast.TokenFor
	tokenWhile     = ast.TokenWhile
	tokenUntil     = ast.TokenUntil
	tokenBreak     = ast.TokenBreak
	tokenNext      = ast.TokenNext
	tokenIn        = ast.TokenIn
	tokenIf        = ast.TokenIf
	tokenUnless    = ast.TokenUnless
	tokenCase      = ast.TokenCase
	tokenWhen      = ast.TokenWhen
	tokenElsif     = ast.TokenElsif
	tokenElse      = ast.TokenElse
	tokenTrue      = ast.TokenTrue
	tokenFalse     = ast.TokenFalse
	tokenNil       = ast.TokenNil
)

func cloneParams(params []Param) []Param            { return ast.CloneParams(params) }
func cloneTypeExpr(ty *TypeExpr) *TypeExpr          { return ast.CloneTypeExpr(ty) }
func cloneStatements(stmts []Statement) []Statement { return ast.CloneStatements(stmts) }

func cloneStringSlice(values []string) []string {
	if values == nil {
		return nil
	}
	out := make([]string, len(values))
	copy(out, values)
	return out
}

// Internal aliases for the value package types so runtime code can keep
// referring to short names (Value, Money, KindInt, NewNil, etc.) without
// repeating the value. prefix everywhere. These mirror the public
// re-exports in vibes/value_alias.go and exist purely to keep the
// runtime sources readable after the move out of package vibes.
type (
	Value           = value.Value
	ValueKind       = value.ValueKind
	EqualityContext = value.EqualityContext
	HashEntry       = value.HashEntry
	Money           = value.Money
	Duration        = value.Duration
	Range           = value.Range
)

type sliceIdentity = value.SliceIdentity

const (
	KindNil       = value.KindNil
	KindBool      = value.KindBool
	KindInt       = value.KindInt
	KindFloat     = value.KindFloat
	KindString    = value.KindString
	KindArray     = value.KindArray
	KindHash      = value.KindHash
	KindFunction  = value.KindFunction
	KindBuiltin   = value.KindBuiltin
	KindMoney     = value.KindMoney
	KindDuration  = value.KindDuration
	KindTime      = value.KindTime
	KindSymbol    = value.KindSymbol
	KindObject    = value.KindObject
	KindRange     = value.KindRange
	KindBlock     = value.KindBlock
	KindEnum      = value.KindEnum
	KindEnumValue = value.KindEnumValue
	KindClass     = value.KindClass
	KindInstance  = value.KindInstance
	KindRegex     = value.KindRegex
	KindShape     = value.KindShape
)

// NewShape returns a first-class shape value wrapping an annotation type
// (ADR-004 expression-position shape literals). The payload is the shared,
// immutable *TypeExpr from the AST.
func NewShape(ty *TypeExpr) Value { return value.NewValue(KindShape, ty) }

// valueShape returns the shape type stored in v, or nil if v is not a shape
// value.
func valueShape(v Value) *TypeExpr {
	if v.Kind() != KindShape {
		return nil
	}
	ty, _ := v.Data().(*TypeExpr)
	return ty
}

// NewNil returns a nil Value.
func NewNil() Value { return value.NewNil() }

// NewBool returns a boolean Value.
func NewBool(b bool) Value { return value.NewBool(b) }

// NewInt returns an integer Value.
func NewInt(i int64) Value { return value.NewInt(i) }

// newBigIntValue returns an integer Value from a big.Int, copying the input
// and normalizing to the compact representation when it fits int64.
func newBigIntValue(i *big.Int) Value { return value.NewBigInt(i) }

// eitherIntPayload reports whether either int operand carries a payload beyond
// the compact scalar (two nil compares; see value.EitherIntPayload).
func eitherIntPayload(a, b Value) bool { return value.EitherIntPayload(a, b) }

// NewFloat returns a floating-point Value.
func NewFloat(f float64) Value { return value.NewFloat(f) }

// NewString returns a string Value.
func NewString(s string) Value { return value.NewString(s) }

// NewArray returns an array Value.
func NewArray(a []Value) Value { return value.NewArray(a) }

// adoptArray wraps a without publishing. The caller already published every
// collection it stored in a.
func adoptArray(a []Value) Value { return value.AdoptArray(a) }

func adoptHash(h map[string]Value) Value { return value.AdoptHash(h) }

// NewHash returns a hash (map) Value.
func NewHash(h map[string]Value) Value { return value.NewHash(h) }

// NewHashWithCapacity returns an empty hash pre-sized for capacity entries.
func NewHashWithCapacity(capacity int) Value { return value.NewHashWithCapacity(capacity) }

// newHashWithOrder returns a hash over entries that iterates in the given key
// order, for the cloners that reuse a shared entry map and must still give each
// wrapper its own order.
func newHashWithOrder(entries map[string]Value, order []Value) Value {
	return value.NewHashWithTrustedOrder(entries, order)
}

// hashIdentity returns a stable identity for a hash wrapper, or 0 when v is not
// a hash. Cloners and scanners key their seen-sets on this rather than the bare
// entry map, so two wrappers sharing one map stay distinct.
func hashIdentity(v Value) uintptr { return value.HashIdentity(v) }

// arrayIdentity returns a stable identity for an array wrapper, or 0 when v is
// not an array. Cloners key their seen-sets on this rather than the element
// backing so aliases of one mutable array clone to one shared object while
// distinct arrays (including independent empties) clone to distinct objects.
func arrayIdentity(v Value) uintptr { return value.ArrayIdentity(v) }

// setArrayElems replaces an array's element slice in place through its shared
// wrapper. It is the primitive behind the Ruby-style in-place mutators (push,
// pop, clear, delete_if, ...) after the write has proved the wrapper exclusively
// held; see collection_values.go.
func setArrayElems(v Value, elems []Value) { v.SetArrayElems(elems) }

// setArrayWindow narrows an array onto a window of the allocation its elements
// already sit in, recording how many slots of that allocation now sit in front
// of the window.
func setArrayWindow(v Value, elems []Value, head int) { v.SetArrayWindow(elems, head) }

// arrayWindowHead reports how many element slots an array's elements start past
// the beginning of the allocation they sit in.
func arrayWindowHead(v Value) int { return value.ArrayWindowHead(v) }

// NewSymbol returns a symbol Value.
func NewSymbol(name string) Value { return value.NewSymbol(name) }

// NewObject returns an object Value with the given attributes.
func NewObject(attrs map[string]Value) Value { return value.NewObject(attrs) }

// ObjectTag records what an attribute bag is, for the few bags the runtime
// builds to stand for something specific.
type ObjectTag = value.ObjectTag

const (
	ObjectTagNone         = value.ObjectTagNone
	ObjectTagRescuedError = value.ObjectTagRescuedError
	ObjectTagMatchData    = value.ObjectTagMatchData
)

// NewTaggedObject returns an attribute bag carrying provenance and the string
// form it publishes, fixed at construction so mutating the entries cannot
// change what it renders.
func NewTaggedObject(attrs map[string]Value, tag ObjectTag, stringForm string) Value {
	return value.NewTaggedObject(attrs, tag, stringForm)
}

// NewMoney returns a money Value.
func NewMoney(m Money) Value { return value.NewMoney(m) }

// NewRegex returns a regex Value.
func NewRegex(r value.Regex) Value { return value.NewRegex(r) }

// NewDuration returns a duration Value.
func NewDuration(d Duration) Value { return value.NewDuration(d) }

// NewTime returns a time Value.
func NewTime(t time.Time) Value { return value.NewTime(t) }

// NewRange returns a range Value.
func NewRange(r Range) Value { return value.NewRange(r) }

func valueToInt64(val Value) (int64, error) { return value.ValueToInt64(val) }

func formatFloat(f float64) string { return value.FormatFloat(f) }

// errStringRenderTruncated re-exports value.ErrStringRenderTruncated so runtime
// callers that render a composite under a byte budget (such as the String#sub /
// gsub block forms bounding a block result before it is spliced into the result)
// can recognize a truncated rendering with errors.Is without importing the value
// package directly.
var errStringRenderTruncated = value.ErrStringRenderTruncated

func parseMoneyLiteral(input string) (Money, error) { return value.ParseMoneyLiteral(input) }

func newMoneyFromCents(cents int64, currency string) (Money, error) {
	return value.NewMoneyFromCents(cents, currency)
}

func parseDurationString(input string) (Duration, error) { return value.ParseDurationString(input) }

func numericToSeconds(val Value) (int64, error) { return value.NumericToSeconds(val) }

func durationFromParts(weeks, days, hours, minutes, seconds int64) Duration {
	return value.DurationFromParts(weeks, days, hours, minutes, seconds)
}

func secondsDuration(v int64, unit string) Duration { return value.SecondsDuration(v, unit) }

func durationFromSeconds(seconds int64) Duration { return value.DurationFromSeconds(seconds) }

func parseLocation(val Value) (*time.Location, error) { return value.ParseLocation(val) }

func parseLocationString(spec string) (*time.Location, error) { return value.ParseLocationString(spec) }

func timeFromParts(args []Value, defaultLoc *time.Location) (time.Time, error) {
	return value.TimeFromParts(args, defaultLoc)
}

func timeFromCalendarParts(args []Value, defaultLoc *time.Location) (time.Time, error) {
	return value.TimeFromCalendarParts(args, defaultLoc)
}

func timeFromEpochParts(secVal Value, subsecVal, unitVal *Value, loc *time.Location) (time.Time, error) {
	return value.TimeFromEpochParts(secVal, subsecVal, unitVal, loc)
}

func parseTimeString(input, layout string, hasLayout bool, loc *time.Location) (time.Time, error) {
	return value.ParseTimeString(input, layout, hasLayout, loc)
}

type hostValueCloneState struct {
	// Each recursive clone borrows disjoint slots until its children return.
	hashScratch *hostHashScratch
	// arrays caches cloned KindArray values keyed on the source array's wrapper
	// identity, so aliases of one mutable array clone to one shared object (a
	// later in-place mutation through one cloned alias stays visible through
	// the others) while independently constructed arrays — including distinct
	// empties — clone to distinct objects. Keying on the element backing would
	// collapse distinct empty arrays onto one clone. This also dedups an array
	// that contains itself.
	arrays map[uintptr]Value
	// hashes caches cloned KindHash values keyed on the source hash's wrapper
	// identity, so a hash reachable through several paths in the returned graph
	// clones to one wrapper and keeps its identity. Caching only the entry map
	// would rebuild a fresh wrapper per path, and since hash identity is the
	// wrapper, the clones would wrongly compare not-identical. This also dedups a
	// hash that contains itself.
	hashes map[uintptr]Value
	// hashEntries caches the cloned entry map keyed on the source hash's entry map
	// pointer. Two distinct hash wrappers may intentionally share one mutable entry
	// map; index assignment mutates that map in place, so both cloned wrappers must
	// point at one cloned entry map to keep the host's aliasing. The wrapper cache
	// cannot do this because the wrappers have distinct identities.
	hashEntries map[uintptr]map[string]Value
	maps        map[uintptr]map[string]Value
	instances   map[*Instance]Value
	classes     map[*ClassDef]*ClassDef
	envs        map[*Env]*Env
	// boundBuiltins caches the clone of a receiver-bound predicate (a bound
	// eql?/equal?) keyed on the source builtin pointer. Cloning such a builtin
	// rebuilds a fresh *Builtin around the cloned receiver, so the same source
	// builtin reachable through several paths in the returned graph would otherwise
	// clone to distinct *Builtin values. equal? compares builtins by backing
	// pointer, so those distinct clones would wrongly report not-identical; caching
	// the clone keeps aliases of one bound predicate identical across the host
	// boundary.
	boundBuiltins map[*Builtin]Value
	// plainBuiltins caches the clone of a plain (non receiver-bound) builtin keyed
	// on the source builtin pointer. cloneBuiltinValue mints a fresh *Builtin for
	// each occurrence, so the same builtin reachable through several paths in the
	// returned graph (for example `p = JSON.parse; [p, p]`) would otherwise clone to
	// distinct *Builtin values. equal? compares builtins by backing pointer, so
	// those distinct clones would wrongly report not-identical; caching the clone
	// keeps aliases of one plain builtin identical across the host boundary, just
	// like the bound-builtin, function, and enum caches above.
	plainBuiltins map[*Builtin]Value
	// functions caches the clone of a script function keyed on the source
	// *ScriptFunction. cloneFunctionForHostWithState rebuilds a fresh
	// *ScriptFunction (with a cloned environment), so the same function reachable
	// through several paths in the returned graph (for example [inc, inc]) would
	// otherwise clone to distinct pointers. equal? compares functions by backing
	// pointer, so those distinct clones would wrongly report not-identical; caching
	// the clone keeps aliases of one function identical across the host boundary.
	// The clone is reserved before its environment is cloned so a function whose
	// captured environment reaches the function itself (a recursive closure)
	// dedups against the reserved clone instead of recursing forever.
	functions map[*ScriptFunction]*ScriptFunction
	// enums caches the clone of an enum definition keyed on the source *EnumDef.
	// cloneEnumDef rebuilds a fresh *EnumDef (and fresh *EnumValueDef members), so
	// the same enum or enum member reachable through several paths in the returned
	// graph (for example [Status::Draft, Status::Draft]) would otherwise clone to
	// distinct pointers. equal? compares enums and members by backing pointer, so
	// those distinct clones would wrongly report not-identical; caching the clone
	// keeps aliases of one enum member identical across the host boundary.
	enums map[*EnumDef]*EnumDef
	// propertyTypes caches the clone of a type expression keyed on the source
	// *TypeExpr. Unlike the caches above this one is not about identity but
	// about size: an unannotated ivar parameter points at the contract its class
	// declares once, and a method mixed in from a module is a shallow copy that
	// leaves every including class reaching the module's own annotations. Both
	// made the clone copy a type the source spells once per parameter and per
	// class (#16).
	propertyTypes ast.TypeExprMemo
}

// valueNeedsHostClone reports whether a Call result must be cloned before it
// crosses to the host, which is what makes doc.go's independent-copy promise
// true. Callables always clone: they alias compiled state the engine keeps.
// A composite clones when any wrapper in its graph is shared -- some other
// durable slot still names it, so handing it out live would let a host write
// through Value.Array or Value.Hash into script-reachable state. A graph of
// sole wrappers has no other owner: its one owner is the call that is ending,
// so the result transfers out as it is and independence costs no copy.
// kindAliasesEngineState reports whether a value of this kind always clones
// at the host boundary: callables, classes, instances, and enums alias
// compiled engine state however they are reached. One predicate feeds both
// the top-level gate and the composite scan, so a new kind cannot be added
// to one and missed by the other.
func kindAliasesEngineState(kind ValueKind) bool {
	switch kind {
	case KindFunction, KindClass, KindInstance, KindEnum, KindEnumValue, KindBlock, KindBuiltin:
		return true
	default:
		return false
	}
}

func valueNeedsHostClone(val Value) bool {
	if kindAliasesEngineState(val.Kind()) {
		return true
	}
	switch val.Kind() {
	case KindArray, KindHash, KindObject:
		return hostBoundaryNeedsClone(val, &hostValueScanState{})
	default:
		return false
	}
}

// hostValueScanState allocates its dedup maps lazily: a cycle needs nesting,
// so a flat result -- the common Call return -- scans without allocating.
type hostValueScanState struct {
	arrays map[uintptr]struct{}
	maps   map[uintptr]struct{}
}

func (s *hostValueScanState) seenArray(id uintptr) bool {
	if id == 0 {
		return false
	}
	if _, ok := s.arrays[id]; ok {
		return true
	}
	if s.arrays == nil {
		s.arrays = make(map[uintptr]struct{})
	}
	s.arrays[id] = struct{}{}
	return false
}

func (s *hostValueScanState) seenMap(ptr uintptr) bool {
	if ptr == 0 {
		return false
	}
	if _, ok := s.maps[ptr]; ok {
		return true
	}
	if s.maps == nil {
		s.maps = make(map[uintptr]struct{})
	}
	s.maps[ptr] = struct{}{}
	return false
}

func hostBoundaryNeedsClone(val Value, state *hostValueScanState) bool {
	if kindAliasesEngineState(val.Kind()) {
		return true
	}
	switch val.Kind() {
	case KindArray:
		// Key on the array wrapper so a cyclic array (one that reaches itself)
		// terminates at the seen check; the wrapper identity is stable even
		// while in-place mutators swap the element backing.
		if state.seenArray(arrayIdentity(val)) {
			return false
		}
		if !val.Unpublished() && !val.SoleRef() {
			return true
		}
		for _, item := range val.Array() {
			if hostBoundaryNeedsClone(item, state) {
				return true
			}
		}
		return false
	case KindHash, KindObject:
		// Key on the whole hash wrapper (or the entry-map pointer for objects) so
		// two wrappers sharing an entry map but carrying distinct defaults are each
		// scanned: a second wrapper's clone-needing default is not skipped, and a
		// default cycling back to this wrapper terminates at the seen check.
		if state.seenMap(hashScanIdentity(val)) {
			return false
		}
		if !val.Unpublished() && !val.SoleRef() {
			return true
		}
		return anyHashValue(val, func(item Value) bool {
			return hostBoundaryNeedsClone(item, state)
		})
	default:
		return false
	}
}

func cloneValueForHost(val Value) Value {
	var scratch hostHashScratch
	state := hostValueCloneState{
		hashScratch:   &scratch,
		arrays:        make(map[uintptr]Value),
		hashes:        make(map[uintptr]Value),
		hashEntries:   make(map[uintptr]map[string]Value),
		maps:          make(map[uintptr]map[string]Value),
		instances:     make(map[*Instance]Value),
		classes:       make(map[*ClassDef]*ClassDef),
		envs:          make(map[*Env]*Env),
		boundBuiltins: make(map[*Builtin]Value),
		plainBuiltins: make(map[*Builtin]Value),
		functions:     make(map[*ScriptFunction]*ScriptFunction),
		enums:         make(map[*EnumDef]*EnumDef),
		propertyTypes: ast.NewTypeExprMemo(),
	}
	return cloneValueForHostWithState(val, state)
}

func cloneValueForHostWithState(val Value, state hostValueCloneState) Value {
	// Scalars fall through the default arm unchanged. That includes a big
	// integer, whose *big.Int payload is deliberately SHARED with the clone
	// rather than copied: big payloads are immutable by construction (see
	// vibes/value/bigint.go — no code mutates a wrapped *big.Int, and the
	// host-facing accessors copy on the way out), so sharing is
	// indistinguishable from copying while keeping host clones O(1) per
	// scalar. A host that digs the live pointer out through Value.Data and
	// mutates it violates that documented contract; Value.BigInt is the safe
	// accessor.
	switch val.Kind() {
	case KindArray:
		items := val.Array()
		id := arrayIdentity(val)
		if id != 0 {
			if clone, ok := state.arrays[id]; ok {
				return clone
			}
		}
		clonedItems := make([]Value, len(items))
		cloned := NewArray(clonedItems)
		if id != 0 {
			state.arrays[id] = cloned
		}
		for i, item := range items {
			clonedItems[i] = cloneValueForHostWithState(item, state)
		}
		publishCollectionElems(clonedItems)
		return cloned
	case KindHash:
		return cloneHostHashValue(val, state)
	case KindObject:
		return cloneHostMapValue(val, state, func(entries map[string]Value) Value {
			return retagClonedObject(val, entries)
		})
	case KindFunction:
		return NewFunction(cloneFunctionForHostWithState(valueFunction(val), state))
	case KindClass:
		return NewClass(cloneClassForHostWithState(valueClass(val), state))
	case KindInstance:
		inst := valueInstance(val)
		if inst == nil {
			return val
		}
		if clone, ok := state.instances[inst]; ok {
			return clone
		}
		clonedClass := inst.Class
		if inst.Class != nil {
			clonedClass = cloneClassForHostWithState(inst.Class, state)
		}
		clonedIvars := make(map[string]Value, len(inst.Ivars))
		cloned := NewInstance(&Instance{Class: clonedClass, Ivars: clonedIvars})
		state.instances[inst] = cloned
		for name, ivar := range inst.Ivars {
			cloned := cloneValueForHostWithState(ivar, state)
			publishCollection(cloned)
			clonedIvars[name] = cloned
		}
		return cloned
	case KindEnum:
		enumDef := valueEnum(val)
		if enumDef == nil {
			return val
		}
		return NewEnum(cloneEnumForHost(enumDef, state))
	case KindEnumValue:
		member := valueEnumValue(val)
		if member == nil || member.Enum == nil {
			return val
		}
		enumClone := cloneEnumForHost(member.Enum, state)
		if memberClone, ok := enumClone.Members[member.Name]; ok {
			return NewEnumValue(memberClone)
		}
		if memberClone, ok := enumClone.MembersByKey[member.Symbol]; ok {
			return NewEnumValue(memberClone)
		}
		return val
	case KindBlock:
		block := valueBlock(val)
		if block == nil {
			return val
		}
		clone := *block
		clone.Params = cloneParams(block.Params)
		clone.ImplicitParams = cloneStringSlice(block.ImplicitParams)
		clone.Body = cloneStatements(block.Body)
		clone.Env = cloneEnvForHost(block.Env, state)
		return value.NewValue(KindBlock, &clone)
	case KindBuiltin:
		return cloneBuiltinForHost(val, state)
	default:
		return val
	}
}

// cloneBuiltinForHost clones a builtin for the host boundary. A receiver-bound
// predicate (one carrying BoundReceiver) is rebuilt around the clone of its
// captured receiver, walked through state so it dedups with the same receiver
// appearing elsewhere in the returned graph. The clone is reserved and cached
// before the receiver is cloned, so a receiver graph that reaches the predicate
// bound to it (for example `[p, a]` where `a` stores `p = a.eql?`) dedups against
// the reserved clone instead of minting a second one the outer call would then
// overwrite — which would make pre-clone aliases report not-identical after the
// boundary. Without rebinding at all the cloned predicate's Fn would keep
// comparing against the pre-clone receiver, so a re-entering probe(clonedReceiver)
// would wrongly report not-identical. Plain builtins have no runtime-cloneable
// state, so they fall back to the shallow copy, memoized on the source builtin so
// aliases of one callable (for example `p = JSON.parse; [p, p]`) stay identical
// across the boundary.
func cloneBuiltinForHost(val Value, state hostValueCloneState) Value {
	builtin := valueBuiltin(val)
	if builtin == nil {
		return cloneBuiltinValue(val)
	}
	if builtin.BoundReceiver == nil {
		if clone, ok := state.plainBuiltins[builtin]; ok {
			return clone
		}
		clone := cloneBuiltinValue(val)
		if state.plainBuiltins != nil {
			state.plainBuiltins[builtin] = clone
		}
		return clone
	}
	if clone, ok := state.boundBuiltins[builtin]; ok {
		return clone
	}
	clone, clonedCell := builtin.BoundReceiver.reserve()
	if state.boundBuiltins != nil {
		state.boundBuiltins[builtin] = clone
	}
	clonedReceiver := cloneValueForHostWithState(builtin.BoundReceiver.receiver.value, state)
	setBoundReceiver(valueBuiltin(clone), clonedCell, clonedReceiver)
	return clone
}

func cloneFunctionForHostWithState(fn *ScriptFunction, state hostValueCloneState) *ScriptFunction {
	if fn == nil {
		return nil
	}
	if clone, ok := state.functions[fn]; ok {
		return clone
	}
	clone := &ScriptFunction{}
	if state.functions != nil {
		state.functions[fn] = clone
	}
	*clone = *fn
	clone.Params = ast.CloneParamsWithTypeMemo(fn.Params, state.propertyTypes)
	clone.ReturnTy = ast.CloneTypeExprWithMemo(fn.ReturnTy, state.propertyTypes)
	clone.Body = cloneStatements(fn.Body)
	clone.Env = cloneEnvForHost(fn.Env, state)
	return clone
}

func cloneClassForHostWithState(classDef *ClassDef, state hostValueCloneState) *ClassDef {
	if classDef == nil {
		return nil
	}
	if clone, ok := state.classes[classDef]; ok {
		return clone
	}
	classClone := &ClassDef{
		Name:          classDef.Name,
		IsModule:      classDef.IsModule,
		Methods:       make(map[string]*ScriptFunction, len(classDef.Methods)),
		ClassMethods:  make(map[string]*ScriptFunction, len(classDef.ClassMethods)),
		ClassVars:     make(map[string]Value, len(classDef.ClassVars)),
		NestedModules: cloneStringSlice(classDef.NestedModules),
		Body:          cloneStatements(classDef.Body),
		owner:         classDef.owner,
	}
	state.classes[classDef] = classClone
	for name, val := range classDef.ClassVars {
		cloned := cloneValueForHostWithState(val, state)
		publishCollection(cloned)
		classClone.ClassVars[name] = cloned
	}
	for methodName, method := range classDef.Methods {
		classClone.Methods[methodName] = cloneFunctionForHostWithState(classDef.bindMethod(method), state)
	}
	for methodName, method := range classDef.ClassMethods {
		classClone.ClassMethods[methodName] = cloneFunctionForHostWithState(classDef.bindMethod(method), state)
	}
	return classClone
}

func cloneEnvForHost(env *Env, state hostValueCloneState) *Env {
	if env == nil {
		return nil
	}
	if clone, ok := state.envs[env]; ok {
		return clone
	}
	clone := newEnvWithCapacity(nil, env.dynamicLen())
	clone.assignBoundary = env.assignBoundary
	clone.rebindOuter = env.rebindOuter
	clone.callRoot = env.callRoot
	// A class body stops the constant-shaped lookup that walks out of it, so
	// the marker has to survive cloning: without it a class-body closure that
	// crosses the host boundary resolves past its own class into the enclosing
	// frames, and a same-named outer local shadows the class constant that
	// should have won (#24).
	clone.classBody = env.classBody
	state.envs[env] = clone
	clone.parent = cloneEnvForHost(env.parent, state)
	if env.declarations != nil {
		clone.declarations = &callDeclarations{script: env.declarations.script, snapshot: true}
	}
	env.rangeDynamicBindings(func(name string, val Value) {
		clone.Define(name, cloneValueForHostWithState(val, state))
	})
	for name, val := range env.statics {
		clone.DefineStatic(name, cloneValueForHostWithState(val, state))
	}
	if env.declarations != nil {
		for name, ref := range env.declarations.classes {
			classClone, reachable := state.classes[ref.Value()]
			if !reachable {
				continue
			}
			if clone.declarations.classes == nil {
				clone.declarations.classes = make(map[string]weak.Pointer[ClassDef])
			}
			clone.declarations.classes[name] = weak.Make(classClone)
		}
		for name, ref := range env.declarations.enums {
			enumClone, reachable := state.enums[ref.Value()]
			if !reachable {
				continue
			}
			if clone.declarations.enums == nil {
				clone.declarations.enums = make(map[string]weak.Pointer[EnumDef])
			}
			clone.declarations.enums[name] = weak.Make(enumClone)
		}
	}
	// A call frame captured by an escaped closure carries the block its method
	// received in a hidden slot; clone it so a closure or default proc that
	// crosses the host boundary still resolves yield and block_given? to that
	// block on re-entry instead of seeing no block.
	if env.hasCallBlock {
		clone.setCallBlock(cloneValueForHostWithState(env.callBlock, state))
	}
	return clone
}

// cloneHostHashValue clones a KindHash value. The cloned hash is cached on its
// source wrapper identity so a hash reachable through several paths (or one
// that contains itself) clones to a single wrapper and keeps its object
// identity across the boundary, and the clone is filled in the source's
// iteration order so the copy iterates the way its source does.
func cloneHostHashValue(val Value, state hostValueCloneState) Value {
	id := hashIdentity(val)
	if id != 0 {
		if clone, ok := state.hashes[id]; ok {
			return clone
		}
	}
	entries := val.HashEntryMap()
	entriesPtr := reflect.ValueOf(entries).Pointer()
	// A distinct wrapper that shares this entry map already cloned it; reuse
	// that cloned map so both cloned wrappers mutate one map in place and the
	// host's intentional aliasing survives the boundary. The shared map is
	// already fully populated, so skip the fill loop -- only a fresh wrapper is
	// built around it.
	sharedEntries, sharedSeen := state.hashEntries[entriesPtr]
	clonedEntries := sharedEntries
	// A shared map is already filled, so the fill loop below is skipped and the
	// wrapper needs this source's order handed to it: two wrappers over one map
	// can iterate differently, and NewHash would derive one order from the
	// map's contents for both. A fresh map is filled entry by entry below,
	// which records the same order as it goes; it is built with the source's
	// reserved capacities up front so the clone keeps the exact shape the
	// builders reserved instead of the append-growth overshoot, and an empty
	// hash keeps the reservation it crossed the boundary with.
	var cloned Value
	if sharedSeen {
		cloned = newHashWithOrder(clonedEntries, val.HashKeyOrder())
	} else {
		cloned = NewHashWithCapacity(value.HashEntryCapacity(val))
		cloned.ReserveHashOrder(value.HashOrderCapacity(val))
		clonedEntries = cloned.HashEntryMap()
	}
	// Register the wrapper before cloning entries so a hash that contains itself
	// dedups against this clone rather than recursing forever or cloning a
	// second wrapper.
	if id != 0 {
		state.hashes[id] = cloned
	}
	if !sharedSeen && entriesPtr != 0 {
		state.hashEntries[entriesPtr] = clonedEntries
	}
	if !sharedSeen {
		// Mirror the source's reserved capacities before filling, so the
		// clone keeps the exact shape the builders reserved (and tests pin)
		// instead of the append-growth overshoot, and an empty hash keeps
		// the reservation it crossed the boundary with.
		cloned.ReserveHashCapacity(value.HashEntryCapacity(val))
		cloned.ReserveHashOrder(value.HashOrderCapacity(val))
		var scratch []value.HashEntry
		if state.hashScratch != nil {
			start := state.hashScratch.used
			if n := val.HashLen(); n <= len(state.hashScratch.entries)-start {
				state.hashScratch.used += n
				scratch = state.hashScratch.entries[start:state.hashScratch.used]
				defer state.hashScratch.release(start)
			}
		}
		for _, entry := range val.HashEntriesInto(scratch[:0]) {
			setClonedHashEntry(cloned, entry.Key, cloneValueForHostWithState(entry.Value, state))
		}
	}
	return cloned
}

type hostHashScratch struct {
	entries [8]value.HashEntry
	used    int
}

func (scratch *hostHashScratch) release(start int) {
	clear(scratch.entries[start:scratch.used])
	scratch.used = start
}

func cloneHostMapValue(val Value, state hostValueCloneState, construct func(map[string]Value) Value) Value {
	entries := val.HashEntryMap()
	ptr := reflect.ValueOf(entries).Pointer()
	if ptr != 0 {
		if clone, ok := state.maps[ptr]; ok {
			return construct(clone)
		}
	}
	clonedEntries := make(map[string]Value, len(entries))
	if ptr != 0 {
		state.maps[ptr] = clonedEntries
	}
	for key, item := range entries {
		clonedEntries[key] = cloneValueForHostWithState(item, state)
	}
	return construct(clonedEntries)
}

func enumOwner(enumDef *EnumDef) *Script {
	if enumDef == nil {
		return nil
	}
	return enumDef.owner
}

// cloneEnumForHost clones an enum definition for the host boundary, memoizing the
// clone on its source *EnumDef so the same enum (and therefore the same members)
// reachable through several paths in one returned graph clones once. equal?
// compares enums and members by backing pointer, so reusing the cached clone keeps
// aliases of one enum member identical after the boundary; cloning per occurrence
// would mint distinct pointers that wrongly report not-identical.
func cloneEnumForHost(enumDef *EnumDef, state hostValueCloneState) *EnumDef {
	if enumDef == nil {
		return nil
	}
	if clone, ok := state.enums[enumDef]; ok {
		return clone
	}
	clone := cloneEnumDef(enumDef, enumOwner(enumDef))
	if state.enums != nil {
		state.enums[enumDef] = clone
	}
	return clone
}

// Builtin represents a built-in function callable from Vibescript. It
// remains defined in the vibes package because BuiltinFunc references
// the runtime *Execution type.
type Builtin struct {
	Name       string
	Fn         BuiltinFunc
	AutoInvoke bool
	// checkSpec carries the static call contract for runtime-owned builtins.
	// Host-registered builtins leave it nil because their contracts are not
	// known to the language checker.
	checkSpec *staticCallSpec
	// SignatureParams carries the published positional parameters of a typed
	// host builtin (NewTypedBuiltin) so argument evaluation applies the same
	// callable and typed expectations an annotated script function would.
	SignatureParams []Param
	// OptionsHashTarget receives a collapsed keyword options hash for builtin
	// wrappers around script functions (method, constructor, and function-call
	// alias callers).
	OptionsHashTarget *ScriptFunction

	// ReturnTypeTarget is the script function whose declared return type
	// governs this builtin's result. Only the wrappers that return their
	// function's value set it, so an absorbed break can be validated against
	// the right annotation.
	//
	// It is deliberately separate from OptionsHashTarget, which records the
	// function an options hash collapses into and is also set on a
	// constructor. A constructor runs initialize through
	// callFunctionIgnoringReturn and returns the instance, so initialize's
	// annotation is not the constructor's contract: reusing that field made
	// `C.new { break 7 }` fail against `def initialize() -> nil`.
	ReturnTypeTarget *ScriptFunction
	// CapturedValues holds runtime values the builtin's Fn closes over and keeps
	// alive for as long as the builtin is reachable. The memory estimator charges
	// their payloads so a stored bound builtin (for example `probe = big.eql?`,
	// which captures its receiver) cannot retain arbitrarily large structures
	// outside the runtime memory quota. Builtins that close over no runtime values
	// leave this nil and stay free, as before.
	CapturedValues []Value
	// BoundReceiver, when non-nil, marks a receiver-bound builtin (such as a bound
	// script method or eql?/equal? predicate) and exposes a two-phase clone. These
	// builtins read the value they were resolved from through a mutable cell, so a
	// plain clone of the Fn keeps using the pre-clone receiver. When
	// Script.Call host-clones a returned graph (or re-roots an inbound one) that
	// holds both a receiver and a predicate bound to it, the clone walk reserves an
	// empty clone, registers it, recurses to clone the receiver, then installs the
	// cloned receiver via this hook. Reserving before recursing keeps a receiver
	// graph that reaches the predicate bound to it (for example `[p, a]` where `a`
	// stores `p = a.eql?`) deduplicated to one clone, so a re-entering
	// `probe(clonedReceiver)` still reports identity. Builtins with no bound
	// receiver leave this nil.
	BoundReceiver *boundReceiverClone
	// Capability marks a builtin a capability adapter exposed for a single
	// Script.Call. Capability grants are per call: when a closure that captured
	// one (for example a `Hash.new { ... }` default proc copying a capability
	// into a local) escapes and re-enters a later call, the inbound rebinder
	// revokes the captured grant so a missing-key lookup cannot invoke a
	// capability the re-entering call never granted.
	Capability bool

	// hostDriven marks a builtin whose Go body the runtime did not write: a
	// capability method, or one registered through Engine.RegisterBuiltin. Such
	// a body may capture an element header from anywhere it can reach before it
	// calls back into the script, and the runtime cannot enumerate what it took,
	// so its frame claims every backing rather than a named one (see
	// array_shrink.go).
	hostDriven bool

	// nonMutating and nonRetaining record the two halves of a builtin's
	// declared contract. Both are promises about the Go body, and both default
	// to the conservative answer: the zero value of a Builtin declares nothing,
	// so a builtin nobody classified keeps the behavior it has today. Omission
	// costs speed, never correctness, which is why every path that rebuilds a
	// Builtin without copying these fields (cloneBuiltinValue, the direct-call
	// alias rebinder, the revoked-capability replacement) stays sound by
	// construction.
	//
	// See declaredNonMutating and declaredNonRetaining for the predicates
	// themselves, which is where the promises are stated in full.
	nonMutating  bool
	nonRetaining bool
}

// declaredNonMutating reports whether this builtin has promised that no
// invocation of it writes to any container reachable from its receiver, its
// arguments, its keyword arguments, its block, or from any execution's roots.
//
// Allocating a fresh container and writing into it is not a mutation under this
// promise: the promise is about state something else can already reach, so a
// builtin that builds a result and returns it keeps the promise. Script code the
// builtin drives through a block is not covered either, and does not need to be:
// a script write advances the mutation epoch on its own.
//
// The runtime relies on the promise in two places, and both are the same claim.
// Dispatch skips the mutation-epoch bump that would otherwise invalidate every
// memoized estimator walk, and a memory check that runs while the builtin is on
// the stack keeps using the memo instead of falling back to a full re-walk.
// Both exist only because a Go body may write through a raw slice or map where
// the epoch cannot observe it; a builtin that declares it does not is exactly
// the case they were guarding against.
func (b *Builtin) declaredNonMutating() bool { return b != nil && b.nonMutating }

// declaredNonRetaining reports whether this builtin has promised that no
// invocation of it stores, anywhere that outlives the invocation, a reference to
// any Value it receives or returns, or to any container reachable from one.
//
// "Anywhere that outlives the invocation" means package-level variables, fields
// on the adapter, closure captures, channels, caches, and anything handed to
// another goroutine. The reachability clause is the part that is easy to get
// wrong: keeping args[0].Hash()["rows"] is retention even though the argument
// itself was not kept.
//
// It is deliberately separate from declaredNonMutating and neither implies the
// other. A builtin that retains without ever writing is still a retainer: the
// container it kept is now reachable from two executions, and script code in
// the second can mutate it in a way attributed to that execution alone. A
// builtin that mutates without retaining is harmless to the retention side and
// fatal to the dispatch side.
func (b *Builtin) declaredNonRetaining() bool { return b != nil && b.nonRetaining }

// BuiltinFunc is the Go function signature for built-in Vibescript functions.
type BuiltinFunc func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error)

// Block represents a closure passed to a function at runtime. It stays
// in the vibes package because its fields reference parser AST and the
// runtime Env/Script types.
type Block struct {
	Params         []Param
	ImplicitParams []string
	Body           []Statement
	Env            *Env
	owner          *Script
	moduleKey      string
	modulePath     string
	moduleRoot     string
	// homeReturnToken identifies the method invocation the block lexically
	// belongs to: a return in the block body returns from that invocation
	// (Ruby non-local return). Zero for host-built or top-level blocks, whose
	// returns report LocalJumpError.
	homeReturnToken uint64
	// retired records that the call this block was attached to has returned.
	// A block is syntax scoped to that one call (ADR-006): it may not be
	// stored, returned, or invoked afterwards. The flag is a shared pointer,
	// not a field value, so every shallow copy a boundary cloner mints keeps
	// pointing at the one lifetime -- retiring the original retires every
	// clone -- and it is atomic because a host may read it from a goroutine
	// the dispatching one races (CallBlock's fail-closed callers). A host
	// that retained the block gets a hard error at the late invocation
	// instead of running a body whose captured frame is gone.
	retired *atomic.Bool
}

// retire marks the block dead because the call it was attached to has
// returned. It is safe on a nil block so callers need no guard.
func (b *Block) retire() {
	if b != nil && b.retired != nil {
		b.retired.Store(true)
	}
}

// isRetired reports whether the block's receiving call has returned. A block
// without a lifetime slot (host-built) never retires.
func (b *Block) isRetired() bool {
	return b != nil && b.retired != nil && b.retired.Load()
}

// retireCallBlock invalidates the block a call was given, once that call has
// returned. Every block literal reaches its callee through one of the call
// evaluators, so retiring there covers every way a block can be supplied.
func retireCallBlock(block Value) {
	valueBlock(block).retire()
}

// NewBlock returns a block (closure) Value.
func NewBlock(params []Param, body []Statement, env *Env) Value {
	return newBlock(params, nil, body, env)
}

func newBlock(params []Param, implicitParams []string, body []Statement, env *Env) Value {
	revokeBlockRegionNeutrality(env)
	return value.NewValue(KindBlock, &Block{Params: params, ImplicitParams: implicitParams, Body: body, Env: env, retired: new(atomic.Bool)})
}

// revokeBlockRegionNeutrality strips epoch neutrality from a scope a closure
// captures, and its enclosing neutral scopes, when the closure is created inside
// an active block-iteration region (see memory_blockregion.go). A neutral scope
// is treated as reachable only through the region's active suffix, which every
// check re-walks fresh — that is what lets its binding writes skip the epoch
// bump. But a closure over it can escape into the memoized prefix (stored in an
// outer binding), after which the scope is reachable from the prefix too: the
// prefix walk folds it into the cached bytes, and the suffix walk then
// deduplicates it against the prefix instead of re-measuring it, so a later
// neutral write to its locals would neither bump the prefix memo nor be
// recharged — undercounting the growth, which on this security boundary is a
// memory-exhaustion escape. Revoking neutrality at capture makes every later
// write to the escapable scope bump, so the prefix is invalidated whenever the
// escaped scope changes. Neutral scopes form a contiguous chain from the
// captured scope up to the region boundary, so the walk stops at the first
// non-neutral (prefix) scope; it is a no-op outside a region.
func revokeBlockRegionNeutrality(env *Env) {
	for scope := env; scope != nil && scope.epochNeutral; scope = scope.parent {
		scope.epochNeutral = false
		// Sticky for the frame's lifetime so a capture during a call frame's
		// pre-push binding survives both the push (which must not restore the
		// neutrality just revoked) and the pre-body memory check's push/pop, after
		// which the same frame is pushed again for the body. Reset per lifetime at
		// frame acquisition (markRegionNeutral), not at pop.
		scope.neutralityRevoked = true
	}
}

// wrapBlock returns a block Value over an existing *Block, used by the inbound
// rebinder to surface a re-rooted block clone without re-deriving the block's
// module metadata.
func wrapBlock(blk *Block) Value {
	return value.NewValue(KindBlock, blk)
}

// NewEnum returns an enum definition Value.
func NewEnum(def *EnumDef) Value { return value.NewValue(KindEnum, def) }

// NewEnumValue returns an enum member Value.
func NewEnumValue(def *EnumValueDef) Value { return value.NewValue(KindEnumValue, def) }

// NewClass returns a class definition Value.
func NewClass(def *ClassDef) Value { return value.NewValue(KindClass, def) }

// NewInstance returns a class instance Value.
func NewInstance(inst *Instance) Value { return value.NewValue(KindInstance, inst) }

// NewFunction returns a script-defined function Value.
func NewFunction(fn *ScriptFunction) Value { return value.NewValue(KindFunction, fn) }

func newBuiltin(name string, fn BuiltinFunc, autoInvoke bool) Value {
	return value.NewValue(KindBuiltin, &Builtin{Name: name, Fn: fn, AutoInvoke: autoInvoke})
}

func newCheckedBuiltin(name string, fn BuiltinFunc, spec staticCallSpec) Value {
	val := newBuiltin(name, fn, false)
	valueBuiltin(val).checkSpec = &spec
	return val
}

// newCheckedAutoBuiltin is newCheckedBuiltin for members that auto-invoke on
// a bare read (Hash.new, Time.mktime). The spec's autoInvoke flag is forced
// on so the static contract never drifts from the runtime dispatch flag.
func newCheckedAutoBuiltin(name string, fn BuiltinFunc, spec staticCallSpec) Value {
	val := newBuiltin(name, fn, true)
	spec.autoInvoke = true
	valueBuiltin(val).checkSpec = &spec
	return val
}

// NewBuiltin returns a builtin function Value.
func NewBuiltin(name string, fn BuiltinFunc) Value { return newBuiltin(name, fn, false) }

// MarkHostBuiltin marks a builtin as one whose Go body the runtime did not
// write, and returns it. The vibes facade applies it to every builtin it hands
// a host, which is the only way a host can make one: internal/runtime is not
// importable from outside this module's own packages.
//
// Marking at construction rather than where a builtin is published is what
// makes it complete. Registration and capability binding only see the callables
// reachable at that moment, so one a host produces later -- a factory'"'"'s result,
// a capability method returning a callable, a builtin returning a builtin --
// stayed unmarked, and dispatch gave its frame no claim over the arrays it
// walks. A block calling pop inside such a frame cleared a slot it had not
// reached, and walking [1, 2, 3] yielded 1, 2, nil.
func MarkHostBuiltin(v Value) Value {
	if b := valueBuiltin(v); b != nil {
		b.hostDriven = true
	}
	return v
}

// DeclareNonMutating records a builtin's promise that no invocation of it
// writes to any container reachable from its receiver, arguments, keyword
// arguments, block, or from any execution's roots, and returns it. Allocating a
// container and filling it in is not such a write; the promise covers only
// state something else can already reach.
//
// This is a safety promise, not a performance hint. The runtime stops
// invalidating its memoized memory-estimator walk around calls to a builtin
// that makes it, so a declaration that is not true leaves an execution's memory
// accounting missing whatever the builtin changed, and the execution then
// allocates past its configured MemoryQuotaBytes. Declare nothing and the
// builtin keeps today's conservative behavior, which is slower and correct.
//
// The promise is between an embedder and itself. A host builtin already runs
// arbitrary Go in the embedding process and can allocate without bound today,
// so declaring grants no capability a host did not have, and script code can
// neither read the declaration nor reach it. What it does do is disable a
// backstop the host is then responsible for honoring.
func DeclareNonMutating(v Value) Value {
	if b := valueBuiltin(v); b != nil {
		b.nonMutating = true
	}
	return v
}

// DeclareNonRetaining records a builtin's promise that no invocation of it
// stores, anywhere that outlives the invocation, a reference to any Value it
// receives or returns, or to any container reachable from one, and returns it.
// Package-level variables, adapter fields, closure captures, channels, caches
// and anything handed to another goroutine all count as outliving it, and
// keeping a container reached through an argument counts as keeping the
// argument.
//
// Host-driven dispatch consults this promise: a builtin that has not made it
// has its collection inputs and result published, so a later script write
// copies instead of mutating a wrapper the host retained.
//
// It is stated as a safety promise rather than a hint because of what it will
// mean once consulted: an execution calling a builtin that makes it keeps
// accounting for memory on its own, so an untrue declaration would let a
// container the host kept be mutated later without that execution observing it,
// and its quota would then admit allocations it should have refused.
//
// It is a separate promise from DeclareNonMutating and neither implies the
// other.
func DeclareNonRetaining(v Value) Value {
	if b := valueBuiltin(v); b != nil {
		b.nonRetaining = true
	}
	return v
}

// NewCapturingBuiltin returns a builtin function Value whose Fn closes over the
// given runtime values. The captured values are recorded on the builtin so the
// memory estimator charges their payloads while the builtin is reachable,
// keeping closures such as a bound predicate's receiver inside the memory quota.
func NewCapturingBuiltin(name string, fn BuiltinFunc, captured ...Value) Value {
	val := newBuiltin(name, fn, false)
	valueBuiltin(val).CapturedValues = captured
	// A captured collection is a durable handle. Leaving it unpublished lets
	// the sole-ref write path mutate a wrapper the builtin still names —
	// `a = [1]; p = a.eql?; a.push(2)` would then change what p compares.
	for _, item := range captured {
		publishCollection(item)
	}
	return val
}

// NewAutoBuiltin returns a builtin function Value that auto-invokes without parentheses.
func NewAutoBuiltin(name string, fn BuiltinFunc) Value { return newBuiltin(name, fn, true) }

// Marker methods bind the runtime payload types to the value.* payload
// interfaces so Value.Class, Value.Builtin, and so on return a typed
// result without forming an import cycle. The names are exported so the
// marker satisfies the interfaces from another package.

func (*Builtin) ValueBuiltinMarker()         {}
func (*Block) ValueBlockMarker()             {}
func (*ClassDef) ValueClassMarker()          {}
func (*Instance) ValueInstanceMarker()       {}
func (*ScriptFunction) ValueFunctionMarker() {}
func (*EnumDef) ValueEnumMarker()            {}
func (*EnumValueDef) ValueEnumValueMarker()  {}

// ClassOf returns the *ClassDef stored in v, or nil if v is not a class
// value. It is the typed companion to v.Class(), which returns the
// value.ClassPayload interface for cycle-free reach from outside vibes.
func ClassOf(v Value) *ClassDef {
	cl, _ := v.Class().(*ClassDef)
	return cl
}

// InstanceOf returns the *Instance stored in v, or nil.
func InstanceOf(v Value) *Instance {
	inst, _ := v.Instance().(*Instance)
	return inst
}

// BlockOf returns the *Block stored in v, or nil.
func BlockOf(v Value) *Block {
	blk, _ := v.Block().(*Block)
	return blk
}

// FunctionOf returns the *ScriptFunction stored in v, or nil.
func FunctionOf(v Value) *ScriptFunction {
	fn, _ := v.Function().(*ScriptFunction)
	return fn
}

// BuiltinOf returns the *Builtin stored in v, or nil.
func BuiltinOf(v Value) *Builtin {
	b, _ := v.Builtin().(*Builtin)
	return b
}

// EnumOf returns the *EnumDef stored in v, or nil.
func EnumOf(v Value) *EnumDef {
	e, _ := v.Enum().(*EnumDef)
	return e
}

// EnumValueOf returns the *EnumValueDef stored in v, or nil.
func EnumValueOf(v Value) *EnumValueDef {
	e, _ := v.EnumValue().(*EnumValueDef)
	return e
}

// The valueX helpers preserve the original short call sites used inside
// the vibes package; new external callers should prefer the exported
// XOf functions above.
func valueClass(v Value) *ClassDef          { return ClassOf(v) }
func valueInstance(v Value) *Instance       { return InstanceOf(v) }
func valueBlock(v Value) *Block             { return BlockOf(v) }
func valueFunction(v Value) *ScriptFunction { return FunctionOf(v) }
func valueBuiltin(v Value) *Builtin         { return BuiltinOf(v) }
func valueEnum(v Value) *EnumDef            { return EnumOf(v) }
func valueEnumValue(v Value) *EnumValueDef  { return EnumValueOf(v) }

// runtimeValueString renders runtime-only value kinds whose payloads live
// in the vibes package. Installed at init time on value.RuntimeStringer.
func runtimeValueString(v Value) (string, bool) {
	switch v.Kind() {
	case KindEnum:
		if enum := valueEnum(v); enum != nil {
			return fmt.Sprintf("<Enum %s>", enum.Name), true
		}
	case KindEnumValue:
		if member := valueEnumValue(v); member != nil && member.Enum != nil {
			return enumMemberText(member), true
		}
	case KindClass:
		if cl := valueClass(v); cl != nil {
			return fmt.Sprintf("<Class %s>", cl.Name), true
		}
	case KindInstance:
		if inst := valueInstance(v); inst != nil && inst.Class != nil {
			return fmt.Sprintf("<%s instance>", inst.Class.Name), true
		}
	case KindShape:
		if ty := valueShape(v); ty != nil {
			return fmt.Sprintf("<Shape %s>", ast.FormatTypeExpr(ty)), true
		}
	}
	return "", false
}

// runtimeValueEqual compares runtime-only value kinds whose payloads live
// in the vibes package. Installed at init time on value.RuntimeEqualer.
func runtimeValueEqual(left, right Value) (bool, bool) {
	switch left.Kind() {
	case KindFunction:
		return valueFunction(left) == valueFunction(right), true
	case KindBuiltin:
		return valueBuiltin(left) == valueBuiltin(right), true
	case KindBlock:
		return valueBlock(left) == valueBlock(right), true
	case KindClass:
		return valueClass(left) == valueClass(right), true
	case KindInstance:
		return valueInstance(left) == valueInstance(right), true
	case KindEnum:
		return enumDefsEqual(valueEnum(left), valueEnum(right)), true
	case KindEnumValue:
		return enumValueDefsEqual(valueEnumValue(left), valueEnumValue(right)), true
	case KindShape:
		return ast.FormatTypeExpr(valueShape(left)) == ast.FormatTypeExpr(valueShape(right)), true
	}
	return false, false
}

// runtimeValueIdentical compares enum and enum-value kinds by backing-pointer
// identity, backing the Ruby-style equal? predicate. Their Equal comparison is
// structural (same owner script and name), so two distinct clones can be Equal
// without sharing storage; identity must instead require the same backing
// pointer. Installed at init time on value.RuntimeIdenticaler.
func runtimeValueIdentical(left, right Value) (bool, bool) {
	switch left.Kind() {
	case KindEnum:
		return valueEnum(left) == valueEnum(right), true
	case KindEnumValue:
		return valueEnumValue(left) == valueEnumValue(right), true
	}
	return false, false
}

func init() {
	value.RuntimeStringer = runtimeValueString
	value.RuntimeStringLen = runtimeValueStringLen
	value.RuntimeStringAppender = runtimeValueStringAppend
	value.RuntimeStringRuneLen = runtimeValueStringRuneLen
	value.RuntimeEqualer = runtimeValueEqual
	value.RuntimeIdenticaler = runtimeValueIdentical
}

// enumMemberText renders an enum member's Enum::Member form into a buffer
// grown once to the exact size, so the peak allocation is the returned string
// and nothing else.
//
// fmt.Sprintf holds a formatting buffer alongside the string it returns, so
// the true peak was roughly twice the text. A guard that charges the text
// length alone therefore passed for a member whose rendering fit the quota
// only narrowly, and the call exceeded it anyway. Both the explicit
// conversions and interpolation render through here, so they cannot drift.
func enumMemberText(member *EnumValueDef) string {
	var sb strings.Builder
	sb.Grow(enumValueRenderingBytes(member))
	sb.WriteString(member.Enum.Name)
	sb.WriteString("::")
	sb.WriteString(member.Name)
	return sb.String()
}

// runtimeValueStringLen reports an enum member's rendered byte length from the
// two identifiers, so a projection can decide whether the rendering fits
// without building it.
func runtimeValueStringLen(v Value) (int, bool) {
	if v.Kind() != KindEnumValue {
		return 0, false
	}
	member := valueEnumValue(v)
	if member == nil || member.Enum == nil {
		return 0, false
	}
	return enumValueRenderingBytes(member), true
}

// runtimeValueStringAppend writes an enum member's text straight into buf, so
// interpolating one holds no temporary alongside the destination the quota
// charged, and a precision-qualified format writes only the bytes it keeps
// rather than rendering the whole member to discard most of it.
//
// limit is the total byte budget for buf, as in appendBounded.
func runtimeValueStringAppend(v Value, buf *strings.Builder, limit int) (truncated, handled bool) {
	if v.Kind() != KindEnumValue {
		return false, false
	}
	member := valueEnumValue(v)
	if member == nil || member.Enum == nil {
		return false, false
	}
	remaining := -1
	if limit > 0 {
		remaining = max(0, limit-buf.Len())
	}
	for _, part := range []string{member.Enum.Name, "::", member.Name} {
		if remaining < 0 {
			buf.WriteString(part)
			continue
		}
		if len(part) > remaining {
			buf.WriteString(part[:remaining])
			return true, true
		}
		buf.WriteString(part)
		remaining -= len(part)
	}
	return false, true
}

// runtimeValueStringRuneLen counts an enum member's rendered runes from the two
// identifiers. Width-qualified formatting projects rune lengths before it
// checks the quota, so counting through Value.String would allocate the very
// rendering the check is meant to gate.
func runtimeValueStringRuneLen(v Value) (int, bool) {
	if v.Kind() != KindEnumValue {
		return 0, false
	}
	member := valueEnumValue(v)
	if member == nil || member.Enum == nil {
		return 0, false
	}
	runes := utf8.RuneCountInString(member.Enum.Name)
	runes += len("::")
	runes += utf8.RuneCountInString(member.Name)
	return runes, true
}

// retagClonedObject preserves an attribute bag's provenance across an internal
// containment clone.
//
// The clones the runtime makes to isolate a value -- across the host boundary,
// when rebinding call arguments -- rebuild every KindObject with
// NewObject, which drops the tag. A match result returned by one Script.Call
// and passed into another therefore rendered as <object> instead of the
// matched text. These clones are the runtime copying its own value, so the
// provenance still holds; a bag that script code rebuilds goes through
// NewObject and still loses it, which is the point of the tag.
func retagClonedObject(src Value, entries map[string]Value) Value {
	if tag := src.ObjectTag(); tag != ObjectTagNone {
		form, _ := src.ObjectStringForm()
		return NewTaggedObject(entries, tag, form)
	}
	return NewObject(entries)
}
