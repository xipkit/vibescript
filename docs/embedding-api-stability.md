# Embedding API Stability Tiers

Status: adopted for the 1.0 freeze. This memo declares which exported Go
symbols the semver contract in `docs/versioning.md` applies to, and which
are internal plumbing that happens to be exported. It also records the
frozen-values design question (#422) as **not implemented, decision
pending**.

The public surface is every non-internal package in the module:
`vibes`, `vibes/value`, `vibes/source`, and `vibes/capability/{contextcap,
db, events, jobqueue}`. `cmd/vibes` and `internal/*` are not covered by any
compatibility promise.

## Why two tiers exist

The interpreter's runtime lives in `internal/runtime`, which imports
`vibes/value`. Go offers no way to export a symbol to one package but not
another, so every hook the runtime needs from `vibes/value` must be a public
export even when no host was ever meant to call it. The standard library
has the same problem and solves it the same way (`testing.Main`: "an
internal function... exposed because it is cross-package"). Rather than
pretend those seams are API, each one is labeled in its doc comment:

> It is intended for the interpreter's internal use; hosts should not call
> it, and it carries no compatibility promise (see
> docs/embedding-api-stability.md).

Post-1.0, Tier 1 follows strict semver. Tier 2 symbols may change shape or
disappear in a MINOR release; they will never change silently in a PATCH.

## Tier 1 — Supported

Everything exported from the public packages that is **not** listed under
Tier 2 below. Concretely:

- **`vibes`** (all of it): `Engine` (`NewEngine`, `MustNewEngine`,
  `Compile`, `CompileSnippet`, `Execute`, `RegisterBuiltin`,
  `RegisterZeroArgBuiltin`, `Builtins`, `ClearModuleCache`,
  `ConfigSummary`, `MaxSourceBytes`), `Config`, `Script` (`Call`,
  `Function`/`Functions`, `Classes`, `Enums`, the `CheckWarnings*`
  family returning `CheckWarning`, `CheckedCall`), `CallOptions`, `Execution` (opaque; `Context`, `Step`,
  `CallBlock`), `RuntimeError`, `StackFrame`, `Position`, `ParseIssue`,
  `ParseIssues`, `Builtin`/`BuiltinFunc`/`Builtins`, `NewBuiltin`,
  `NewAutoBuiltin`, `ParamKind` and the `Param*` constants,
  `MemberCompletionNames`, the capability adapter surface
  (`CapabilityAdapter`, `CapabilityBinding`, `CapabilityContractProvider`,
  `CapabilityMethodContract`, `New*Capability`, `MustNew*Capability`).
  `CapabilityMethodContract` carries only `ValidateArgs` and
  `ValidateReturn`; the pre-rc8 `ReturnValidatedByBuiltin` field was
  removed (#976) because a host adapter must not be able to assert an
  internal runtime proof and skip its declared return validation. The
  runtime now always runs a contract's `ValidateReturn`; only first-party
  builtins inside the runtime can record the internal proof that a result
  was already validated. Migration: delete the field from contract
  literals — no replacement is needed, the extra validation pass is the
  intended behavior.
- **`vibes/value`**: `Value`, `ValueKind` and the `Kind*` constants; the
  typed constructors (`NewNil` ... `NewRegex`, `NewBigInt`, `NewHash`,
  `NewMoneyFromCents`); the typed accessors
  (`Kind`, `IsNil`, `Truthy`, `Bool`, `Int`, `Float`, `BigInt`, `IsBigInt`,
  `String`, `Inspect`, `Array`, `Money`, `Duration`, `Time`, `Range`,
  `Regex`, `Data`); the hash surface (`Hash`, `HashGet`, `HashSet`, `HashLen`,
  `HashEntries`, `HashDeleteKey`, `HashClearEntries`, `HashEntry`);
  equality (`Equal`, `Eql`, `Identical`, `Truthy`); the opaque payload
  markers (`BlockPayload`,
  `BuiltinPayload`, `ClassPayload`, `EnumPayload`, `EnumValuePayload`,
  `FunctionPayload`, `InstancePayload`) and their accessors; bounded
  rendering (`StringBounded`, `InspectBounded`,
  `ErrStringRenderTruncated`, `ErrStringRenderDepthExceeded`); bounded rendering
  rejects descent beyond 16,384 nested composites even with no byte budget;
  the domain scalars (`Money`, `Duration`,
  `Range`, `Regex`) and their methods and constructors; the conversion
  and parsing helpers (`FormatFloat`, `ValueToInt64`, `NumericToSeconds`,
  `ParseMoneyLiteral`, `ParseDurationString`, `SecondsDuration`,
  `ParseTimeString`, `DefaultTimeParseLayouts`, `ParseLocation`,
  `ParseLocationString`, `TimeFromParts`, `TimeFromCalendarParts`,
  `TimeFromEpochParts`).
- **`vibes/source`**: `Position`, `FormatCodeFrame`, `CodeFrameFormatter`,
  `NewCodeFrameFormatter`.
- **`vibes/capability/*`**: everything (these packages exist solely for
  hosts). The `Capability` types, the host interfaces (`Database`,
  `DatabaseReader`, `DatabaseWriter`, `Publisher`, `JobQueue`,
  `JobQueueWithRetry`, `Resolver`), the request structs, contract
  validators, and `ParseEnqueueOptions`/`ParseEnqueueOptionsValidated`.

Two structural notes:

1. Several `vibes` names are type aliases for `internal/runtime` types.
   The contract is the alias-visible surface — the exported methods and
   fields reachable through `vibes.Engine`, `vibes.Script`, etc. — not the
   `internal/runtime` package, which hosts cannot import.
2. Some methods return values of exported-but-unnameable internal types
   (`Script.Functions` → `*runtime.ScriptFunction`, `Script.Classes`).
   Hosts can hold these only through inference (`:=`) and use their
   exported fields and methods; that reachable surface is Tier 1, the type
   names are not. Checker diagnostics are the exception: the
   `CheckWarnings*` family returns `vibes.CheckWarning` (a stable public
   alias with `Function`, `Pos`, `Message`, and `Source`), so hosts can
   name the diagnostic type in their own APIs.

## Tier 2 — Internal, no compatibility promise

Exported from `vibes/value` only because `internal/runtime` needs them.
Every one carries the doc-comment label quoted above. Most were exported
during the 2026-06/07 mutator, hash-order, and estimator-memoization work
(#867, #873, #895, #905), not for any host request.

| Group | Symbols | Runtime need |
| --- | --- | --- |
| Runtime hooks | `RuntimeStringer`, `RuntimeEqualer`, `RuntimeIdenticaler`, `NewValue` | format/compare runtime-only kinds whose payload types live outside `vibes/value` |
| Mutation epoch | `MutationEpoch`, `BumpMutationEpoch` | invalidate the memory estimator's memoized base walk (#905) |
| Identity | `ArrayIdentity`, `HashIdentity`, `ObjectIdentity`, `SliceIdentity`, `EqualityContext` | cycle detection / seen-sets during graph walks; representation identity is internal |
| Quota accounting | `HashDataBytes`, `ArrayDataBytes`, `HashOrderCapacity` | charge wrapper and reservation bytes to the memory quota |
| Wrapper mutation | `SetArrayElems`, `SetArrayWindow`, `ArrayWindowHead`, `ReserveHashOrder`, `ReserveHashOrderUnpublished`, `HashSetUnpublished` | primitives behind Ruby-style in-place mutators and clone bookkeeping (#873, #895); the window pair lets an in-place shrink tell how much of an allocation it has left behind, which a slice header does not say; the unpublished pair is the literal builder's epoch-free write |
| Hash constructors | `NewHashWithOrder`, `NewHashWithTrustedOrder`, `NewHashWithCapacity` | rebuild a hash with a recorded or trusted key order, or pre-size an empty one |
| Hash iteration | `HashEntriesInto`, `RangeHashEntries`, `HashKeyOrder`, `HashUsesRecordedOrder`, `HashKeyString`, `HashEntryMap`, `HashIterationScratchBytes` | string-keyspace walks, the recorded-order check stringify uses to fill its own buffer, and the internal map reader that does not mark a hash host-exposed |
| Rendering projections | `StringByteLen`, `StringRuneLen`, `StringByteLenBounded`, `StringRuneLenBounded`, `StringByteLenBoundedUpTo`, `InspectByteLenBounded`, `WriteStringTo`, `WriteInspectTo` | sandbox interpolation/inspect memory guards project output size before allocating |
| Object provenance | `ObjectTag`, `ObjectTagNone`, `ObjectTagRescuedError`, `ObjectTagMatchData`, `NewTaggedObject`, `Value.ObjectTag`, `Value.ObjectStringForm`, `ObjectDataBytes` | mark the two bags the runtime builds to stand for something specific (a rescued error, match data) so their `to_s` renders without inferring it from field names |
| Big-integer plumbing | `AdoptBigInt`, `Value.CompactInt`, `BigIntPayload`, `BigIntDecimalLenUpperBound` | copy-free promotion in arithmetic, quota accounting, and rendering preflight for big-integer payloads (#919); hosts use `NewBigInt`/`BigInt`/`IsBigInt` |

`HashDeleteKey` and `HashClearEntries` were exported in the same #895 batch
but are kept in Tier 1 deliberately: they complete the host-facing hash
family (`HashGet`/`HashSet`/`HashLen`/`HashEntries`) with clean Ruby
`Hash#delete`/`Hash#clear` semantics and are safe on any Value a host owns.

Two orphaned exports (`TimeFromEpoch`, superseded by `TimeFromEpochParts`;
`InspectByteLen`, superseded by `InspectByteLenBounded`) were removed
outright in the freeze audit rather than tiered — nothing in the module or
in any documented host workflow used them.

## Ownership, concurrency, and quota model (probe-verified)

Verified empirically across the module boundary by an external host
program (see the `vibes/value` package doc for the host-facing wording):

- Results of `Script.Call` are independent copies; mutating them through
  any Tier 1 or Tier 2 primitive never changes what a later Call observes.
  One sharing note: an out-of-int64-range integer's `*big.Int` payload is
  shared, not copied, across the clone boundary. This is safe because big
  payloads are immutable by contract — nothing on either side mutates a
  wrapped `*big.Int`, and `Value.BigInt` copies on the way out — so the
  sharing is unobservable through the supported surface. `Value.Data` is
  the one escape hatch that exposes the live pointer; mutating it corrupts
  the value (its doc says so) and, for a value that came out of a Call, can
  corrupt what a later Call observes. Treat `Data`'s big payload as
  read-only and use `BigInt` for an owned copy.
- Arguments and `CallOptions.Globals` are never mutated by the script;
  mutating them **between** calls is safe and the next call sees the new
  contents.
- Mutating a Value **during** a Call that received it is forbidden. This
  is not a stale-data hazard: globals materialize lazily inside the call,
  and the resulting concurrent map access kills the host process with an
  unrecoverable `fatal error: concurrent map read and map write`.
- Host-side mutation is unmetered: no step or memory quota observes it,
  and host-only graphs are never charged to a later call (the estimator
  walks only execution-reachable state; epoch churn merely costs a memo
  refresh — nothing persists execution-to-execution).
- `ArrayIdentity`/`HashIdentity`/`ObjectIdentity` return bare `uintptr`s. A
  wrapper that never escapes its host function can be stack-allocated, and a
  goroutine stack copy relocates it, changing the identity of a live value.
  Only the runtime's single-traversal usage is safe. Wrapper identity is not a
  supported host operation: `Value.Identical` compares collection contents,
  not wrappers. A host that needs provenance must track it separately rather
  than infer it from Vibescript's runtime storage.

## Frozen values (#422) — NOT implemented, decision pending

#422 (Script.Call argument rebinding cost) listed "expose a documented
immutable/frozen host value path if hosts want to opt into safe reuse" as
a candidate direction before it was closed by the #900 call-boundary copy
reductions. Sketch, for a future decision:

```go
frozen := value.Freeze(payload) // deep-freeze; error on callables/cycles
res, err := script.Call(ctx, "run", []value.Value{frozen}, opts)
```

- `value.Freeze(v)` deep-walks v, verifies it is data-only and acyclic
  (the walk the capability boundary already performs), and sets a frozen
  bit on every array/hash wrapper. Scalars are already immutable.
- The Call entry path shares frozen subgraphs by reference instead of
  scanning/cloning them; an alternative shape is a `CallOptions` flag
  declaring all globals frozen, but the per-value form composes better
  with mixed payloads.

Isolation implications under reference-mutable collections — this is the
expensive part, not the freeze walk:

- Today isolation holds **by construction**: both directions copy, so no
  mutator can be wrong. Sharing frozen wrappers converts that into
  isolation **by enforcement**: every script-side in-place mutator
  (`push`, `[]=`, `delete`, `clear`, `delete_if`, ...) must check the frozen
  bit and raise a `FrozenError` (Ruby precedent), and every Tier 2
  host-side primitive must refuse frozen wrappers too, or a host mutation
  concurrent with a running call reintroduces the process-fatal race
  documented above. One missed mutator path is a silent host-state
  corruption bug of exactly the class the #895 containment tests exist to
  prevent.
- The memory estimator must decide whether shared frozen graphs are
  charged per call (hosts pay quota for their own payload, surprising) or
  exempted (cheap, and safe because a fresh root env per call means
  scripts cannot retain references across calls). Frozen graphs never bump
  the mutation epoch, which is a synergy: estimator memos stay valid.
- Language surface grows: `FrozenError`, `frozen?`, and documentation of
  which builtins raise.

Cost/benefit today: #900 already fast-paths data-only graphs and binds
globals lazily, so the residual win is skipping the per-call data-only
scan on very large, reused payloads — real but narrow, and quantifiable
with `BenchmarkExecutionGroupByHashRowsLowCardinality` when the question
returns. The enforcement surface, by contrast, touches every mutator in
the interpreter.

**Recommendation: defer.** Ship 1.0 without freezing; `value.Freeze` plus
a `CallOptions` opt-in is purely additive and can land in any MINOR
release if profiling ever shows the entry scan dominating a production
workload. Nothing in the current Tier 1 surface forecloses it.
