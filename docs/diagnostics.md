# Source diagnostics

Compile and execution failures expose an `Error` with a category, a bare message, and optional source context. `offset` is a zero-based byte offset. `diagnostic.position` contains a one-based line and Unicode character column; `diagnostic.code_frame` is a bounded source snippet, and `diagnostic.frames` contains the complete script call trace.

```rust
use vibescript::{CallOptions, Engine, ErrorKind, Position, Value};

fn main() -> vibescript::Result<()> {
    let script = Engine::new().compile("def divide(a, b)\n  a / b\nend")?;
    let error = script.call("divide", &[Value::int(1), Value::int(0)], CallOptions::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    assert_eq!(error.message, "division by zero");
    assert_eq!(error.diagnostic.as_ref().unwrap().position, Position { line: 2, column: 5 });
    Ok(())
}
```

Printing an error includes its message, code frame and call trace. Parse errors start with `parse error at line:column` and have no call frames. Errors raised before source execution, such as an unknown entry function or a pre-cancelled call, may have no diagnostic. Oversized source and compiler errors without a meaningful token position also omit the snippet.

Argument and return checks point to the calling expression. Default expressions retain their own source positions, and a function enters the trace when its body begins. Blocks use the active function call stack. Top-level code and namespace initialization do not expose internal VM function names. Interpolations and implicit `to_s` calls retain their original source positions. Index and range validation uses the compiled expression or assignment position, and typed block bindings use the binding name; these can differ from Go's operand-level locations. Negative numeric receivers include their sign, and an incomplete construct at end of input points to that boundary. The parser currently reports its first failure.

Code frames show at most 160 source characters with clipping markers. Tabs are preserved in the caret indentation. Displayed traces longer than sixteen frames show eight frames from each end and an omitted-frame count; the structured trace remains complete.

Each compiled script retains its source and a sparse position index, plus a four-byte source offset per bytecode instruction. Position checkpoints occur roughly every 4 KiB of source, including long lines. Diagnostic objects retain a snippet and shared function names; they hold no compiled script, host callback, argument or runtime value. Cloned errors share the diagnostic. Diagnostic formatting happens after execution fails and is outside the script's allocation counters. Retaining an error therefore does not retain a call's runtime allocations.

This is the source-context foundation for language error handling. `raise`, `rescue`, `else`, `ensure`, `retry` and script-visible error objects remain pending.
