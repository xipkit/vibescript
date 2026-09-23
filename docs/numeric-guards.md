# Numeric bounds and recovery

Unary plus is the identity for integers, floats and strings. String bytes and their backing storage are preserved, including non-UTF-8 host input. Other operand types retain their existing errors.

Integer arithmetic supports arbitrary precision. Products of long integers use Karatsuba multiplication, and operands of at least 128 words convolve through number-theoretic transforms modulo 2^64 - 2^32 + 1. Division estimates one quotient word at a time until the divisor and quotient both reach 256 words and the longer 1024; longer divisions go through Newton reciprocals and Barrett reduction. Powers of two are set directly. Parsing splits long digit strings in halves around a power of the radix, rendering in other radixes recombines halves in limbs of a power of the radix, and power-of-two radixes convert bits directly in both directions. Work is charged per word operation, counting each transform butterfly as one, so long products and quotients cost O(n log n) word operations and parsing and rendering O(n log^2 n) in the number of digits. Iteration counts, limits and strides must fit the iterator's 64-bit representation. Oversized values raise a recoverable `LimitError`; validation order follows each method's contract:

| Operation | Validation before the oversized-value guard |
| --- | --- |
| `int.times` | Positional arity and required block |
| `int.upto`, `int.downto` | Arity, keywords, integer limit type and required block |
| `int.step` | Arity, keywords and integer limit type; oversized values precede stride and block validation |
| `range.step` | Arity, keywords and integer stride type; oversized strides precede block and open-range validation |

`range.to_a` raises `LimitError` when its element count cannot fit the supported representation. `range.size` overflow remains a `RuntimeError`. Bounded `first(n)` reads from an endless range stop at the signed-integer ceiling: `(9223372036854775807..).first(3)` returns `[9223372036854775807]`. Bounded reads from a full-width finite range remain supported.

`array.fill` rejects an overflowing window before allocating its result or invoking its block. Negative counts still leave the array unchanged. Range selectors preserve their existing bounds and clamping rules.

Use an explicit filter to recover from these limits:

```vibescript
begin
  0.step(9223372036854775808)
rescue LimitError => error
  error.type # "LimitError"
end
```

A plain rescue catches `RuntimeError`. Exhaustion of the current invocation's step or memory budget, cancellation and deadlines remain uncatchable, including by an explicit `LimitError` clause.

Mutations retain their previous binding until publication while a handler or type guard is active. A rejected update therefore leaves its local, nested, class or instance binding available to rescue and ensure. Prior completed statements and explicit writes inside a block remain visible; an unfinished `fill` result is not published. Retaining the previous value can require copying and additional tracked temporary memory for successful writes in handled regions.
