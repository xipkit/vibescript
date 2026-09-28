# Error handling

`begin` expressions and function bodies support ordered `rescue` clauses, `else`, `ensure`, and `retry`. A successful body returns its last value; a handled failure returns the selected rescue body's value. `else` runs only after normal completion of the body. `ensure` runs before an ordinary error or return, break, or next leaves the protected region. Its value is ignored, but an error or control transfer from ensure replaces the pending outcome.

Rejected native mutations preserve their receiver bindings for rescue and ensure. Previously completed statements and explicit block writes remain visible. See [numeric guards](numeric-guards.md) for recoverable bounds, mutation publication and the accounting implications.

```vibe
def run -> array<string>
  begin
    raise TypeError, "wrong value"
  rescue TypeError => error
    [error.class, error.message, "#{error}"]
  end
end
```

This returns `["TypeError", "wrong value", "wrong value"]`. The rescued value has type `error`.

Clauses match in source order. Filters accept canonical exception names, the `Error` alias, unions such as `TypeError | ArgumentError`, and parenthesized or nullable forms. An omitted filter uses `StandardError`, which excludes `LimitError`. `RuntimeError` matches every script exception class. An empty matching clause consumes selection and propagates the original error after ensure.

`JSON.parse_as` raises `TypeError` when valid JSON does not fit the requested type, just as `.as(T)` does for a failed cast. Malformed JSON raises `RuntimeError`. A Rust `ErrorKind::Type` alone does not tell you which script class to rescue. A capability adapter can publish its failure class with `Error::with_class`; consult the host's contract. See [typed input validation](types.md#type-mismatch-diagnostics).

The binding after `=>` shadows an outer local only inside that clause. Other assignments in the body belong to the surrounding scope, but a local the protected body assigns is read after the `begin` only when every rescue clause assigns it too (V0202); otherwise assign it before, or use the `begin` expression's value.

A same-line rescue modifier supplies a fallback for an expression or a call without parentheses:

```vibe
def run -> any
  JSON.parse("{") rescue { ok: false }
end
```

`raise "message"` creates a RuntimeError. Two operands specify a class and a string message. Bare `raise` rethrows the current rescued error, including when called by a helper. Outside rescue it raises an empty RuntimeError. `raise error` does not accept a rescued object. `assert(condition, message)` takes a `bool` condition, returns nil when it is true and raises AssertionError otherwise; the default message is `assertion failed`.

`retry` restarts the protected body without running that handler's ensure between attempts. Each attempt consumes work. Nested ensures run when retry exits their regions. Retry cannot cross a function or block call boundary; a rescue inside a block can retry its own body. Invalid return, break, and next transfers become LocalJumpError only after the callee's cleanup has run. Break and next outside any loop or block reject before evaluating a value operand.

The `begin` expression's type includes every branch that produces a value.
A rescue ending in `retry` contributes no value or `nil`: this expression is
an `int`, the type of its successful body.

```vibe run
def run -> [int, int, int]
  attempts = 0
  cleanups = 0
  value: int = begin
    attempts += 1
    raise "again" if attempts < 3
    42
  rescue
    retry
  ensure
    cleanups += 1
  end
  [value, attempts, cleanups]
end
assert run == [42, 3, 1]
```

This returns `[42, 3, 1]`.

Rescued errors expose `backtrace`, `class`, `code_frame` and `message`, and interpolation renders the message. The removed spellings `type` and `to_s` are rewritten to `class` and `message`. Nested writes and duplicates preserve protection and special rendering, including after host transfer. Message strings preserve arbitrary bytes; the Rust host API provides `Error::message_bytes()` for those bytes, while `message` and Display replace invalid UTF-8 for display.

Saved errors, bound objects, handler storage, pending return values and diagnostic capacities remain accounted while execution continues. Resuming after failure releases discarded call frames, temporary arguments, addresses and interpolation buffers. Error diagnostics retain no script or host callback. See [source diagnostics](diagnostics.md) for position and trace conventions.

Actual invocation exhaustion, cancellation and deadlines cannot be rescued. A previously exhausted invocation cannot execute ensure statements. An ordinary failure first followed by exhaustion inside ensure reports that exhaustion at the ensure operation. Fixed operation guards and explicitly raised LimitError values remain recoverable when the invocation still has budget. A foreign error's category does not establish that the current invocation has exhausted its resources.

A rescue modifier cannot select a call target in the static language; see [computed calls](computed-calls.md). [Required files](require.md) include filenames in source diagnostics.
