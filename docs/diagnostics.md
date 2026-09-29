# Source diagnostics

Compilation reports stable coded diagnostics before any script runs. Use `vibes check --json script.vibe` for one JSON object per finding: `code`, `message`, `span`, `expected`, `found`, related `labels`, and `fixes`. Spans use UTF-8 byte offsets plus one-based line and character columns. Diagnostics from required files identify that file. The [code registry](checker.md#diagnostics) describes each code.

A fix includes a message, an applicability and a list of text edits. `always` means tools can apply it automatically; `suggestion` requires choosing a repair. `vibes fix` applies only `always` edits, rechecks and repeats. Read labels and suggestions as well as the primary message. Unknown names use V0201/V0203; V04xx specifically identifies removed Vibescript spellings, not every spelling borrowed from another language.

For V0107, bind an optional read to a local and test that local with `!= nil`. Arrays, dictionaries and regex match data can instead use `fetch` when absence should raise; strings have no `fetch`, and fetching a nullable collection element does not remove its nil possibility. V0114 names the qualified enum members needed in a `when`, such as `State::Closed`.

V0201 adds a note for common Ruby, Python, JavaScript and Go names: `len(x)` uses `x.length`, `fmt.Sprintf(pattern, ...)` uses `format(pattern, ...)`, and `strings.ToLower(text)` uses `text.downcase`. These notes are related `labels` in JSON diagnostics. They apply only when the name is undefined; scripts and hosts may still declare these names. The curated table lives in `src/typing/foreign.rs`. Most entries offer advice only because argument conventions, conversion failures or formatting behavior can differ. `len` with one positional argument of a known array, tuple, dictionary or record type offers a machine-applicable `length` edit; strings get a note because character and byte counts differ between languages.

V0003's fix for a suffixed binding renames every place that binds it and every read of it in scope, written `nil_` where the name is a keyword, so one fix migrates the binding. It never renames a name that no suffixed binding owns: an undefined `ready?`, such as a host function the check cannot see, keeps its V0201 without a fix.

## Execution failures

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

Adjacent expressions such as `x = 1"0"` report V0001 at the gap. The message suggests an operator, a comma between arguments, or a newline or `;` between statements. No automatic edit is offered because the intended repair is ambiguous. Calls without parentheses still accept their arguments.

Host compilation recovers at statement, declaration and delimiter boundaries and reports independent syntax errors together through `Error::diagnostics()`, `vibes check --json` and the language server. Syntax errors use V0001, except that a hash argument mistaken for a block has V0002 and a parenthesis fix. The first error keeps its original message and position. Type checking begins only when the entire file parses. Recovery stops after 100 diagnostics, a syntax-depth failure, or its source-proportional work bound; quota-limited host compilation also obeys its caller's limits, deadline and cancellation. Cold `require` compilation remains fail-fast so script-visible accounting and quota outcomes are unchanged. An error at the end of input is located at the last character, or at column 0 of the next line when the source ends with a line break. Computed call targets are rejected with V0310; use a direct function or method call. See [computed calls](computed-calls.md).

Code frames show at most 160 source characters with clipping markers. Tabs are preserved in the caret indentation. Displayed traces longer than sixteen frames show eight frames from each end and an omitted-frame count; the structured trace remains complete.

Each compiled script retains its source and a sparse position index, plus a four-byte source offset per bytecode instruction. Position checkpoints occur roughly every 4 KiB of source, including long lines. Diagnostic objects retain a snippet, shared function names and filename bytes; they hold no compiled script, filesystem root, host callback, argument or runtime value. Cloned errors share the diagnostic. Terminal runtime diagnostic formatting happens after execution fails and is outside the script's allocation counters. Cold required-file compilation instead charges error messages, diagnostic headers, filename storage retained by the diagnostic and source excerpts before construction, because the receiving script can rescue a parse failure. Diagnostic counting and writing obey work limits, cancellation and deadlines. Partial failures release their storage, and the first latched termination remains authoritative. Errors saved while a rescue or ensure can resume execution instead charge diagnostic capacity and formatting work to the invocation, counting shared filename storage once per saved diagnostic. Retaining a terminal error does not retain a call's runtime allocations. Raised messages preserve their original bytes through `Error::message_bytes()`; the public `message` string and Display replace invalid UTF-8.

`Error::class()` exposes the script exception class independently of `ErrorKind`. In particular, a `JSON.parse_as` schema mismatch and a failed `.as(T)` cast both have kind `Type` and class `TypeError`; malformed JSON has kind `Json` and class `RuntimeError`. For example, an unsupported addition has class `RuntimeError` and kind `Type`, while incompatible relational operands have class `ArgumentError` with the same kind. Script argument binding uses `ArgumentError`, and integer division by zero uses `ZeroDivisionError`. Host-side compilation errors, cancellation and deadlines have no script class. Syntax failures while loading a required file retain kind `Syntax` and their file diagnostics, but have class `RuntimeError` so the receiving script can rescue them. Hosts can attach a class with `Error::with_class`; this does not change the invocation's budget state.

Fixed input, output, recursion and value-depth guards report `LimitError` without exhausting the invocation. A host callback may handle such a rejected operation and continue within the remaining budget. Actual step or memory exhaustion, the materialized `string.scan` output cap, cancellation and deadlines remain latched. Once latched, the original termination wins even if a callback returns a different error. An error returned from another invocation does not prove that the current invocation has spent its budget.

`raise`, `rescue`, `else`, `ensure`, `retry`, `assert` and protected script-visible error objects are implemented; see [error handling](errors.md). [Computed call targets](computed-calls.md), including a function selected by a rescue expression, are rejected at compile time (V0310). Arrays, hashes and JSON values support [10,000 nested containers](json-depth.md).
