# Architecture decision records

ADR-001 through ADR-006 were written for the Go implementation, and are copied
here unchanged except for status lines. Their relative links point into that
repository's `docs/` tree. From ADR-007 on, decisions are made for this
implementation, which is the reference going forward.

| ADR | Decision | Status |
| --- | --- | --- |
| [001](001-tasks-structured-concurrency.md) | Tasks for bounded structured concurrency | Superseded by 006 |
| [002](002-cli-quota-profiles.md) | Named quota profiles and an `xhigh` CLI default | Accepted |
| [003](003-bignum-arbitrary-precision-integers.md) | Arbitrary-precision integers with transparent promotion | Accepted |
| [004](004-static-checking-for-typed-boundaries.md) | Infer local types and check typed boundaries statically | Superseded by 007 |
| [005](005-dev-mode-module-reloading.md) | Dev-mode module reloading | Accepted |
| [006](006-slim-language-for-predictable-sandboxing.md) | Slim the language for predictable sandboxing | Accepted |
| [007](007-static-types.md) | Static types with local inference | Accepted |
| [008](008-canonical-surface-for-ai-authors.md) | A canonical surface for AI authors | Accepted |
