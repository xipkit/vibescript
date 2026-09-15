# Error handling

`begin` expressions and function bodies support ordered `rescue` clauses, `else`, `ensure`, and `retry`. A successful body returns its last value; a handled failure returns the selected rescue body's value. `else` runs only after normal completion of the body. `ensure` runs before an ordinary error or return, break, or next leaves the protected region. Its value is ignored, but an error or control transfer from ensure replaces the pending outcome.

```vibescript
def run(input)
  begin
    raise TypeError, "wrong value"
  rescue TypeError => error
    [error.type, error.message, "#{error}"]
  end
end
```

Clauses match in source order. Filters accept canonical exception names, the `Error` alias, unions such as `TypeError | ArgumentError`, and parenthesized or nullable forms. An omitted filter uses `StandardError`, which excludes `LimitError`. `RuntimeError` matches every script exception class. An empty matching clause consumes selection and propagates the original error after ensure.

The binding after `=>` shadows an outer local only inside that clause. Other assignments in the body belong to the surrounding scope. A skipped clause's assigned locals are declared as nil before a later matching clause executes.

A same-line rescue modifier supplies a fallback for an expression or a call without parentheses:

```vibescript
def run(input)
  JSON.parse("{") rescue {ok: false}
end
```

`raise "message"` creates a RuntimeError. Two operands specify a class and a string message. Bare `raise` rethrows the current rescued error, including when called by a helper. Outside rescue it raises an empty RuntimeError. `raise error` does not accept a rescued object. `assert(condition, message)` returns nil for a truthy condition and raises AssertionError otherwise; `message:` is also accepted, and the default message is `assertion failed`.

`retry` restarts the protected body without running that handler's ensure between attempts. Each attempt consumes work. Nested ensures run when retry exits their regions. Retry cannot cross a function or block call boundary; a rescue inside a block can retry its own body. Invalid return, break, and next transfers become LocalJumpError only after the callee's cleanup has run. Break and next outside any loop or block reject before evaluating a value operand.

```vibescript
def run(input)
  attempts = 0
  cleanups = 0
  value = begin
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
```

This returns `[42, 3, 1]`.

Rescued objects expose `type`, `class`, `message`, `to_s`, `code_frame`, and `backtrace`. Interpolation and `to_s` render the message. Nested writes and duplicates preserve protection and special rendering, including after host transfer. Message strings preserve arbitrary bytes; the Rust host API provides `Error::message_bytes()` for those bytes, while `message` and Display replace invalid UTF-8 for display.

Saved errors, bound objects, handler storage, pending return values and diagnostic capacities remain accounted while execution continues. Resuming after failure releases discarded call frames, temporary arguments, addresses and interpolation buffers. Error diagnostics retain no script or host callback. See [source diagnostics](diagnostics.md) for position and trace conventions.

Actual invocation exhaustion, cancellation and deadlines cannot be rescued. A previously exhausted invocation cannot execute ensure statements. An ordinary failure first followed by exhaustion inside ensure reports that exhaustion at the ensure operation. Fixed operation guards and explicitly raised LimitError values remain recoverable when the invocation still has budget. A foreign error's category does not establish that the current invocation has exhausted its resources.

Rescue-selected call targets, including `(missing rescue fallback)()`, are supported; their handlers finish before argument evaluation. See [computed calls](computed-calls.md) for receiver binding, the selected begin-call parsing policy and the remaining namespace-error difference. Diagnostic wording and some source locations differ from Go as described in the diagnostic guide. File loading, source filenames and the broader language completion requirements also remain pending.
