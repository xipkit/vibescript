# Output helpers

The engine provides `puts`, `print`, `warn`, and `p`. Hosts configure their byte writers before compiling a script.

The CLI configures stdout and stderr automatically and prints its final JSON result after script output.

| Helper | Writer | Rendering | After each argument | Return value |
| --- | --- | --- | --- | --- |
| `puts` | Output | String form | Newline | `nil` |
| `print` | Output | String form | Nothing | `nil` |
| `warn` | Error | String form | Newline | `nil` |
| `p` | Output | Inspection | Newline | `nil`, its single argument, or an array of its arguments |

`puts` with no arguments writes one newline. The other helpers write nothing with no arguments. Every helper requires its writer to be configured, including empty calls. Arguments run before validation; keywords are rejected first, then blocks, then missing writers. A bare helper name is a call, so `puts` alone writes one newline; the helpers are never values. Local parameters, registered host functions, and ordinary function declarations follow the normal name resolution rules.

```vibe
puts "hello", 7
print "x", "y"
warn "careful"
p({ a: [1, "two"] })
```

The output stream receives `hello\n7\nxy{a: [1, "two"]}\n`, and the error stream receives `careful\n`. The final expression returns `{a: [1, "two"]}`. The returned collections retain logical value semantics; changing a returned copy does not change an earlier collection. Class instances retain their normal identity semantics, and protected values retain their tags.

String rendering preserves raw bytes, uses an empty string for nil, and prints symbols without a colon. Inspection uses quoted strings, colon-prefixed symbols, and `nil`. Its escaping rules match [value inspection](rendering.md). Neither operation requires valid UTF-8.

For a direct class instance, `puts`, `print`, and `warn` run an eligible `to_s` method through the VM. The method must have no required positional or keyword arguments. Private methods and defaults participate normally. A string return supplies the output; another return type uses the default instance representation. Return annotations are still checked. Nested instances inside collections use the default representation. `p` always inspects values without invoking `to_s`.

Match data and rescued errors retain their existing Rust string rendering inside collections and copies. Inspection shows their fields. The seven output differences from Go's object placeholders are recorded in [the output comparison cases](output-differences.json). Six type-message differences now have exact shared expectations; eight other argument-message differences remain tracked separately in that file.

Each rendered argument is limited to 1 MiB, excluding the helper's added newline. Inspection quotes and escapes count toward the limit. The cap is per argument, so several valid arguments can produce more than 1 MiB in total. Exceeding the cap raises a recoverable `LimitError`. Output buffers, pending arguments, conversion scratch, and class calls remain accounted. Actual work or memory exhaustion, cancellation, and deadlines remain uncatchable.

```rust
use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine};

let output = Arc::new(Mutex::new(Vec::<u8>::new()));
let captured = output.clone();
let mut engine = Engine::new();
engine.set_output_writer(move |ctx, bytes| {
    ctx.checkpoint()?;
    captured.lock().unwrap().extend_from_slice(bytes);
    Ok(())
});
let script = engine.compile("puts(\"hello\")").unwrap();
script.run(CallOptions::default()).unwrap();
assert_eq!(*output.lock().unwrap(), b"hello\n");
```

`Engine::set_error_writer` configures `warn` with the same callback contract. Configuration is captured when a script is compiled. Callbacks can run concurrently, must write the whole supplied slice or return an error, and must cooperate with their `CallContext`. Stored output belongs to the host. The engine checks the context before and after every writer call, including when the callback ignores a quota error or returns another error.

Arguments are written in order, with one callback per rendered argument; an empty rendered argument can produce an empty slice for `print`. Output already accepted by a writer remains visible if a later render or write fails. Ordinary writer errors can be rescued. Resuming or retrying a script follows normal control flow and does not roll back earlier writes.
