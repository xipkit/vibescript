# Source diagnostics

Compile and execution failures expose an `Error` with a category, a bare message, and optional source context. `offset` is a zero-based byte offset. `diagnostic.position` contains a one-based line and Unicode character column; `diagnostic.code_frame` is a bounded source snippet, and `diagnostic.frames` contains the complete script call trace. Required files set `diagnostic.filename` and each frame's `filename` to shared root-relative filename bytes. Sources compiled directly by the host have no filename. The bytes preserve filesystem spelling, including non-UTF-8 names.

```rust
use vibescript::{CallOptions, Engine, ErrorClass, ErrorKind, Position, Value};

fn main() -> vibescript::Result<()> {
    let script = Engine::new().compile("def divide(a: int, b: int) -> int\n  a // b\nend")?;
    let error = script.call("divide", &[Value::int(1), Value::int(0)], CallOptions::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    assert_eq!(error.class(), Some(ErrorClass::ZeroDivision));
    assert_eq!(error.message, "division by zero");
    assert_eq!(error.diagnostic.as_ref().unwrap().position, Position { line: 2, column: 5 });
    Ok(())
}
```

Printing an error includes its message, code frame and call trace. Parse errors start with `parse error at line:column` and have no call frames. Errors raised before source execution, such as an unknown entry function or a pre-cancelled call, may have no diagnostic. Oversized source and compiler errors without a meaningful token position also omit the snippet.

Named locations render as `pkg/helper.vibe:line:column` in parse errors, code frames, printed traces and rescued error backtraces. Each frame names the source of its position: a required function's failure can point into the module while its calling expression points into another file or an unnamed host script. Default expressions and blocks keep their own source, and imported modules retain their original filenames. Display escapes filename backslashes, control characters and invalid UTF-8 bytes; structured filenames retain their exact bytes.

Argument and return checks point to the calling expression. Default expressions retain their own source positions, and a function enters the trace when its body begins. Blocks use the active function call stack. Top-level code and namespace initialization do not expose internal VM function names. Interpolations and implicit `to_s` calls retain their original source positions. Index and range validation uses the compiled expression or assignment position, and typed block bindings use the binding name. A negative number such as `-5` is located at its digits.

The parser stops at its first error. Its messages and positions began as those of Go v0.70.0's first error line, which the port matched on every case in `tests/syntax-errors.json` and on mutated copies of the corpus programs. The `parse` [golden corpus](../tests/golden/README.md) records the first error on copies of the site and upstream programs with a token deleted or duplicated, a stray `)`, `]`, `}`, `=`, `,`, `end`, `do`, `|`, `:` or `.` inserted, or the source cut off after a line. An error at the end of input is located at the last character, or at column 0 of the next line when the source ends with a line break. Parentheses after a `begin` expression that spans lines call its value (the [begin-call policy](computed-calls.md)), where Go ended the expression at the line break.

Code frames show at most 160 source characters with clipping markers. Tabs are preserved in the caret indentation. Displayed traces longer than sixteen frames show eight frames from each end and an omitted-frame count; the structured trace remains complete.

Each compiled script retains its source and a sparse position index, plus a four-byte source offset per bytecode instruction. Position checkpoints occur roughly every 4 KiB of source, including long lines. Diagnostic objects retain a snippet, shared function names and filename bytes; they hold no compiled script, filesystem root, host callback, argument or runtime value. Cloned errors share the diagnostic. Terminal runtime diagnostic formatting happens after execution fails and is outside the script's allocation counters. Cold required-file compilation instead charges error messages, diagnostic headers, filename storage retained by the diagnostic and source excerpts before construction, because the receiving script can rescue a parse failure. Diagnostic counting and writing obey work limits, cancellation and deadlines. Partial failures release their storage, and the first latched termination remains authoritative. Errors saved while a rescue or ensure can resume execution instead charge diagnostic capacity and formatting work to the invocation, counting shared filename storage once per saved diagnostic. Retaining a terminal error does not retain a call's runtime allocations. Raised messages preserve their original bytes through `Error::message_bytes()`; the public `message` string and Display replace invalid UTF-8.

`Error::class()` exposes the script exception class independently of `ErrorKind`. For example, an unsupported addition has class `RuntimeError` and kind `Type`, while incompatible relational operands have class `ArgumentError` with the same kind. Script argument binding uses `ArgumentError`, and integer division by zero uses `ZeroDivisionError`. Host-side compilation errors, cancellation and deadlines have no script class. Syntax failures while loading a required file retain kind `Syntax` and their file diagnostics, but have class `RuntimeError` so the receiving script can rescue them. Hosts can attach a class with `Error::with_class`; this does not change the invocation's budget state.

Fixed input, output, recursion and value-depth guards report `LimitError` without exhausting the invocation. A host callback may handle such a rejected operation and continue within the remaining budget. Actual step or memory exhaustion, the materialized `string.scan` output cap, cancellation and deadlines remain latched. Once latched, the original termination wins even if a callback returns a different error. An error returned from another invocation does not prove that the current invocation has spent its budget.

`raise`, `rescue`, `else`, `ensure`, `retry`, `assert` and protected script-visible error objects are implemented; see [error handling](errors.md). [Computed call targets](computed-calls.md), including a function selected by a rescue expression, are supported. Arrays, hashes and JSON values support [10,000 nested containers](json-depth.md).
