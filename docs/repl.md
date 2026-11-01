# Bounded embedded REPLs

`vibescript_tools::repl::ReplSession` provides the terminal-independent session used by `vibes repl`. `ReplOptions::limits` applies to each execution, not the whole session. `ReplOptions::session_limits` opts into retained-text and compilation limits:

```rust
use vibescript_tools::repl::{ReplOptions, ReplSession, SessionLimits};

let mut session = ReplSession::new(ReplOptions {
    session_limits: SessionLimits {
        pending_input_bytes: Some(64 << 10),
        history_bytes: Some(256 << 10),
        transcript_bytes: Some(1 << 20),
        compile_steps: Some(10_000_000),
        compile_memory_bytes: Some(16 << 20),
        ..SessionLimits::default()
    },
    ..ReplOptions::default()
});
session.feed_line("answer = 42");
```

The input limit counts UTF-8 bytes and joining newlines, including complete inputs submitted through `evaluate`. Admission happens before copying a submitted line; an overflow drops pending input and returns a compile error with an empty input field, so the rejected source is not retained. History counts input text plus one `String` header per input. Transcript counts input and output text plus one `Entry` header per entry, including command output and completion listings. Storage caps evict the oldest entries and skip a single entry larger than its cap. Zero disables retention. Returned evaluations still contain their normal results and output when an entry is skipped.

The compile step allowance is cumulative: incomplete-input probes, declaration parsing, compilation and namespace preparation with carried declarations, type annotation passes and failed attempts all spend it. `compilation_steps()` reports the total. Exhaustion stays latched until `reset()` or `:reset`. `compile_memory_bytes` bounds tracked compiler allocations, and `compile_deadline` is an optional absolute monotonic deadline. The session cancellation token and per-input `interrupter()` also stop compilation; cancelling an interrupter permits the next input to run with a fresh token, without renewing the step allowance. Reset renews steps but does not extend the deadline or undo whole-session cancellation.

`discard_pending()` releases an unfinished input; `clear_history()` releases recall history; `clear_transcript()` and `:clear` release transcript storage. Reset drops variables, declarations, pending input and the last error while preserving useful history and transcript entries under their caps. Clearing text does not renew compiler work.

All session limits default to unlimited, including in the CLI. These are embedding robustness controls, not an aggregate memory sandbox: vector spare capacity, generated binding/source text, the engine clone, returned responses, temporary output, variables, carried declarations and host allocations are outside the text caps and compiler memory account. Hosts still need a policy for those resources and for the input buffers they own. This API makes no claim about an untrusted remote REPL deployment.

Hosts building their own sessions can use `vibescript::CompileBudget` to share a `CallOptions` work allowance between `compile`, `declaration_names`, `retain_namespace` and `type_check` passes. It exposes `stats()` even after failure and never returns partial type findings on exhaustion. `Engine::compile_with_options` remains the convenience API for a single compilation.
