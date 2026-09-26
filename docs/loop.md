# Repeated block execution

`loop { ... }` runs its block until `break`. A break value becomes the result, and a bare `break` gives `nil`; the signature is `loop(&block: ()) -> any`, so narrow the result or declare the local it is stored in. `next` starts another iteration, discarding its value, and ordinary block results are discarded too. A `return` leaves the enclosing function, and every control transfer honors `ensure` clauses.

```vibe
count = 0
result = loop {
  count += 1
  next if count < 3
  break count * 10
}
result # 30
```

The block receives no arguments. Calls reject positional arguments and keywords, and `loop` without a block is a compile error (V0304). Script functions can shadow the global helper under the usual name-resolution rules.

`loop` is a direct call target and cannot be read as a value. Its native frame holds the block and call roots for the duration of execution, charges work on every iteration and uses the recursion limit. Discarded results are released before the next block call; retained results continue consuming the memory budget. Cancellation and exhausted limits remain uncatchable and prevent subsequent rescue or ensure effects.

A `{` after a call starts its block, so a block's first statement never reads as a hash entry: `loop { break :done }` breaks with the symbol, where Go v0.70.0 read the braces as a hash literal and failed with a syntax error. The [difference record](loop-differences.json) lists this spelling.
