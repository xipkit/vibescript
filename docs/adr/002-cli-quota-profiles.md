# ADR-002: Named quota profiles and an `xhigh` CLI default

## Status

Accepted - 2026-07-08

## Decision

We will add named quota profiles that bundle the three execution quotas — step,
memory, and recursion — into a single coherent budget, and an explicit
`Unlimited` quota spelling that is distinct from an unset (zero) quota.

Four profiles form an ascending ladder: `ProfileLow`, `ProfileMedium`,
`ProfileHigh`, and `ProfileXHigh`. The `vibes` CLI (`vibes run`, `vibes test`,
and the REPL) will default to `xhigh` — unlimited steps and memory — so it runs
the developer's own scripts like a normal interpreter. No profile leaves
recursion uncapped: even `xhigh` keeps a finite recursion limit.

The zero-value `Config` default resolves to the `low` profile, so an embedder
that sets no quotas gets the bottom rung of the ladder as its budget, and `low`
is the reproducible name for the default embedding sandbox.

## Context

The embedding `Config` exposes the three quotas as independent integer knobs.
Two problems followed from that shape.

First, a coherent budget is hard to hit by hand. A host that wants a "generous
but bounded" run has to pick three correlated numbers, and a mismatched trio
(huge step quota, tiny memory quota) fails in confusing ways. There was no
vocabulary for "a normal sandbox budget" versus "run it wide open."

Second, zero was overloaded. A zero quota means "use the engine's conservative
built-in default" — the `low` profile (1,000,000 steps / 16 MiB / 256
recursion), the lowest named sandbox budget. That left no way to express
"explicitly unbounded." A host could only tighten a ceiling, never lift one.

Those defaults are correct for an embedded sandbox but wrong for the CLI. The
CLI runs the developer's own scripts on the developer's own machine; it is not a
sandbox. CPU-heavy scripts — deep recursion, long loops, `fib(35)` — tripped the
bounded embedding defaults out of the box, which is a poor first-run experience
for a tool that is supposed to just run your code.

## API Shape

A profile is a named bundle of the three quotas:

```go
type QuotaProfile struct {
    Name             string
    StepQuota        int
    MemoryQuotaBytes int
    RecursionLimit   int
}
```

The four profiles, in ascending order of generosity:

| Profile              | Step quota  | Memory quota | Recursion | Intended use                          |
| -------------------- | ----------- | ------------ | --------- | ------------------------------------- |
| `ProfileLow`         | 1,000,000   | 16 MiB       | 256       | tight, untrusted embedded budget      |
| `ProfileMedium`      | 20,000,000  | 128 MiB      | 1,000     | moderate embedded budget              |
| `ProfileHigh`        | 200,000,000 | 512 MiB      | 4,000     | generous embedded budget              |
| `ProfileXHigh`       | unlimited   | unlimited    | 10,000    | run like a normal interpreter         |

Quota values follow the same conventions as `Config`: a positive value is an
explicit limit, `Unlimited` disables that quota, and zero selects the built-in
default. `ApplyTo(*Config)` writes only the three quota fields, leaving every
other `Config` field untouched, so a host can select a profile and then layer
explicit per-quota overrides on top. `QuotaProfileByName` resolves a
case-insensitive name (for a flag or config file) and `QuotaProfileNames`
enumerates the ladder for help text.

The CLI selects a profile with `-profile low|medium|high|xhigh` and defaults to
`xhigh`. Individual quotas can be overridden on top of the selected profile with
`-step-quota`, `-memory-quota`, and `-recursion-limit`, where `-1` means
unlimited. This is how a developer reproduces sandbox behaviour locally, for
example running under `xhigh` but with a real memory cap.

## Semantics

Recursion is never uncapped, deliberately. The interpreter recurses on the host
Go stack, so an unbounded recursion would crash the process with an uncatchable
stack overflow instead of a clean `recursion depth exceeded` error. Every
profile therefore keeps a finite recursion cap — high enough to be irrelevant to
any real program, low enough to fail cleanly on runaway recursion. `xhigh` sets
it to 10,000.

An unlimited memory quota short-circuits the reachable-graph accounting walk
entirely: the per-check estimator only runs when a finite memory quota is in
force. So `xhigh` is not only unbounded, it also skips the sandbox's per-check
memory-accounting cost, which is why it is the right default for trusted local
runs. The corollary is that memory accounting — a security boundary for
untrusted scripts — is off under `xhigh`. That is acceptable precisely because
the CLI is not a sandbox; an embedding that needs the boundary must set a finite
memory quota (a lower profile or an explicit `MemoryQuotaBytes`).

The profile list is a single ordered slice; lookup and name enumeration both
derive from it, so the ladder and its lookups cannot drift.

## Non-goals

- No per-operation, per-capability, or per-module budgets; profiles bundle only
  the three existing whole-execution quotas.
- No auto-detection or adaptive scaling of quotas to the host machine; a run
  must be reproducible across machines.
- Profiles do not touch non-quota `Config` fields (module policy, strict
  effects, source-size limits); `ApplyTo` writes only the three quota fields.
- Profiles do not silently override a host's explicit quotas: an embedder that
  sets `StepQuota`/`MemoryQuotaBytes`/`RecursionLimit` keeps those values.
  Selecting or applying a profile is opt-in; only an unset (zero) quota resolves
  to the default budget (the `low` profile — see Decision).

## Consequences

The CLI runs CPU-heavy scripts out of the box while embedders get a safe,
conservative default — the `low` profile. A host now has a short vocabulary —
`low`/`medium`/`high`/`xhigh` — for a coherent budget, plus the ability to lift a
ceiling, not only tighten it. Adding a profile or tuning its values is a one-line
change in a single slice.

Because the zero-value default was raised to the `low` profile, a host that
relied on the previous tighter default (50,000 steps / 64 KiB / 64 recursion)
must now set those quotas explicitly; the migration is a one-line `Config` change
or a lower explicit value.

The main tradeoff is that `xhigh` disables memory accounting, so the default CLI
run does not enforce the memory boundary. This is documented and intentional,
but it means anyone measuring or relying on sandbox behaviour must select a
finite memory quota rather than assume the default enforces one.

## Alternatives Considered

### Make the CLI default fully unlimited, including recursion

Rejected. Interpreter recursion runs on the Go stack; an unbounded recursion
limit turns a script bug into an uncatchable process crash instead of a clean
`recursion depth exceeded` error. Steps and memory can be unlimited safely;
recursion cannot.

### Keep only raw per-quota flags, no named bundles

Rejected. A coherent budget across three correlated numbers is error-prone to
set by hand and opaque to read. A name is self-documenting and keeps the three
values consistent. The raw overrides remain available on top of a profile.

### Overload zero to mean unlimited

Rejected. Zero already means "use the conservative built-in default" in `Config`.
Conflating unset with unbounded would remove the ability to distinguish the two
and would make a safe default impossible to express. A separate `Unlimited`
sentinel keeps both meanings.

### Auto-scale quotas to the host machine

Rejected. Machine-derived quotas make a run non-reproducible: the same script
would pass on one machine and trip a quota on another. Explicit profiles keep
behaviour predictable and portable.
