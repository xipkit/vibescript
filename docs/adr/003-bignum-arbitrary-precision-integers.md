# ADR-003: Arbitrary-precision integers with transparent promotion

## Status

Accepted - 2026-07-08

## Decision

Vibescript will expose a single script-visible integer type whose values are
arbitrary-precision. A value that fits in a signed 64-bit range is always held
in a compact scalar form; a value outside that range carries an immutable
`*big.Int` payload. Every arithmetic producer normalizes its result back to the
compact form when it fits, so the two representations are disjoint: no in-range
value is ever a big payload, and no big payload is ever an in-range value.

Integer arithmetic (`+ - * / % **`, unary minus) that would previously overflow
now promotes into a big integer instead of raising `result out of int64 range`.
This matches Ruby, which unified `Fixnum` and `Bignum` into one `Integer`.

## Context

Vibescript targets Ruby-compatible semantics for non-technical authors and AI
code generation. Ruby integers are arbitrary-precision: `2 ** 200` and a running
factorial just work. Before this change Vibescript had int64-only integers that
raised `result out of int64 range` on overflow (the v0.50.0 behaviour).

That was the wrong failure mode on three counts:

- **Correctness surprise.** Ordinary-looking arithmetic — factorials, large
  sums, combinatorics, hashing math — raised an error a Ruby author would never
  see. The ceiling was invisible until a script tripped it in production.
- **Representation gaps.** Integer literals beyond int64 failed to parse
  (`invalid integer literal`), and `JSON.parse` silently degraded oversized
  integer tokens to floats, losing exactness.
- **No safe wrap.** The only alternatives to erroring were C-style silent
  wraparound — a correctness and security footgun in a sandbox — or forcing
  authors to reach for a separate big-number library, which defeats the
  "reads like Ruby" goal.

At the same time, integers are the hottest values in the interpreter. Whatever
we chose could not tax the common in-range path, and it had to keep equality,
identity, and hash-key behaviour trivially correct, because integers are used as
hash keys and set members everywhere.

## Design

**One type, two representations, a canonical invariant.** The compact scalar
form is the hot path and is untouched: in-range arithmetic allocates nothing and
boxes nothing. Only out-of-range values carry a `*big.Int`. Because every
constructor normalizes back to compact when a value fits, the value spaces never
overlap — `x - x` round-trips to a compact `0`, and a big payload can never equal
a compact value by construction. That invariant is what keeps `==`, `eql?`,
identity, and hash keys consistent for free: equal big values compare and hash
equal without ever colliding with compact keys, and there is no "is `5` the same
as `big(5)`" ambiguity to adjudicate because `big(5)` cannot exist.

**The payload is charged and bounded.** A big integer is real host memory, so the
memory estimator charges its struct plus word backing, deduped by payload
identity and journaled like every other reachable identity — big integers are
inside the memory-quota security boundary, not an escape from it. Operations that
could explode a payload (`2 ** 10_000_000_000`, oversized base conversions in
renderers and projections) preflight the projected digit count and refuse or
step-charge the work *before* running the superlinear conversion, so a script
cannot use bignum growth as a memory or CPU exhaustion vector.

**Semantics are pinned to Ruby.** Division and modulo keep floor semantics for
big operands, matched to Ruby 3.4's sign matrix via truncated `QuoRem` plus a
divisor-sign adjustment (`big.Int.DivMod` is Euclidean and deliberately avoided).
Comparisons are exact across representations: big versus compact orders by sign
(the spaces are disjoint), and big versus float compares through `big.Float` with
no precision loss, so `sort`/`min`/`max`, `uniq`, `tally`/`group_by`, and hash
lookups all treat equal values as equal.

**Bounded domains still say no.** The int64-only domains — range endpoints, array
indexes and counts, and `Duration`/`Time`/`Money` operands — reject big integers
loudly with their existing error conventions instead of truncating through
`Value.Int`. Promotion is transparent for arithmetic, but a value that is
semantically an index or a size stays bounded.

## API Shape

The Tier 1 host API stays small: `NewBigInt`, `Value.BigInt`, and
`Value.IsBigInt`, with internal-use plumbing (`AdoptBigInt`, `CompactInt`,
`BigIntPayload`) for the runtime. Scripts see no new type or class — an integer
is an integer; promotion is invisible.

## Non-goals

- No separate `Fixnum`/`Bignum` classes exposed to scripts. Ruby merged them in
  2.4; two visible types would leak representation into semantics.
- No arbitrary-precision rationals or decimals. `Money` and `Time` keep their
  fixed resolution deliberately; this ADR is about integers only.
- Big integers are not permitted as indexes, sizes, or range/`Duration`/`Time`/
  `Money` operands. Those domains remain int64-bounded and error loudly.
- No unbounded bignum work inside the sandbox: huge payloads are preflighted
  against the quotas, not computed and then measured.

## Consequences

The common case is unchanged and free: in-range integer math still runs on the
compact scalar path with no allocation. Authors get Ruby's mental model —
integers do not overflow — and literals and JSON round-trip exact large values.

The change removed a runtime error, which is a breaking change for any script
that caught `result out of int64 range`; the migration is documented in
[docs/migrating-to-1.0.md](../migrating-to-1.0.md) and shipped in v1.0.0-rc2.

The cost is internal complexity: the value type now has two representations, and
every arithmetic producer must normalize, every comparison must handle the cross
-representation cases, and every payload must be charged and preflighted. That
complexity is contained inside the value and runtime layers and is the price of
keeping the hot path fast while the language stays correct.

## Alternatives Considered

### Keep int64-only, error on overflow (the prior behaviour)

Rejected. It is Ruby-incompatible, surprises authors with an invisible ceiling,
and cannot represent large literals or exact large JSON integers.

### Silent wraparound (C-style)

Rejected. Silent corruption is the worst outcome in a sandbox meant to run
semi-trusted logic; a wrong number is a correctness and security hole.

### Represent every integer as `*big.Int`

Rejected. It would allocate and box the hottest value in the interpreter on
every operation and complicate equality, identity, and hashing. The compact/big
split keeps the common case allocation-free and the invariant simple.

### Expose separate `Fixnum` and `Bignum` script types

Rejected. Ruby itself abandoned that split. Two visible types would force authors
to reason about representation and would make `5 == big(5)` a question the
language has to answer, rather than one the canonical invariant makes impossible.

### Add rationals/decimals in the same change

Rejected as out of scope. Integer overflow is the concrete footgun; rationals are
a separate, larger design. `Money` and `Time` intentionally keep fixed
resolution.
