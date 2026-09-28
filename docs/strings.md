# String operations

Strings contain arbitrary bytes and preserve value semantics. These methods complement [string iteration](string-iteration.md), [regular expressions](regex.md) and [inspection and templates](rendering.md).

## Concatenation and conversion

Binary `+` joins two strings. Any other operand is converted explicitly, with `to_s` or interpolation: `"r=" + (1..3).to_s` and `"r=#{1..3}"` both return `"r=1..3"`, and `+` between a string and a number or range is a compile error (V0108).

Binary `*` repeats a string. A float count truncates toward zero, so `"ab" * 2.5` returns `"abab"` and `"ab" * -0.5` returns `""`. A negative truncated count, a non-finite float or one outside the 64-bit range is an error.

`concat` accepts any number of strings and returns their concatenation in argument order. The receiver's binding remains unchanged. With no arguments, or only empty strings, it reuses the receiver.

```vibe
text = "he"
result = text.concat("llo", "!")
[text, result] # ["he", "hello!"]
```

`to_s` returns the string itself. `to_sym` returns a symbol with the same bytes, including empty strings, invalid UTF-8 and embedded zero bytes. Symbols remain distinct from strings in equality. Both conversions take no arguments.

On a symbol, `to_s` returns its bytes as a string and `to_sym` returns the symbol itself. The removed spellings `string`, `intern` and `id2name` are rewritten to `to_s` and `to_sym`. Conversions share immutable storage within a call and retain its memory charge until the last owner is released.

```vibe
text = "status"
symbol = text.to_sym
[symbol == :status, symbol == text, symbol.to_s] # [true, false, "status"]
```

`to_i` parses a complete, trimmed decimal integer and supports arbitrary precision. `to_f` parses a complete finite decimal or hexadecimal floating-point number. Invalid text, nonfinite values and floating-point overflow are errors. Both take no arguments.

`hex` and `oct` instead consume a numeric prefix after optional ASCII whitespace and a sign. Underscores may separate digits; parsing stops at the first invalid character. Missing digits return zero, and results outside signed 64-bit range are errors. `hex` uses base 16 and accepts `0x`. `oct` defaults to base 8 and recognizes `0b`, `0o`, `0d` and `0x`.

```vibe
["42".to_i, "0x1.8p+1".to_f, "ff tail".hex, "0b101".oct]
# [42, 3, 255, 5]
```

## Bounds and searching

`clamp(min: string?, max: string?) -> string` compares byte strings and permits `nil` for an unbounded end. An inverted interval is an error. `between?(min: string, max: string) -> bool` includes both endpoints; it stops once the lower bound excludes the receiver.

```vibe
["a".clamp("m", "z"), "zz".clamp(nil, "z"), "m".between?("a", "z")]
# ["m", "z", true]
```

`index(text: string, offset: int = 0) -> int?` and `rindex(text: string, offset?: int) -> int?` return a character position or `nil`. A negative offset counts from the end. Forward search starts at the offset; reverse search considers matches starting at or before it and defaults to the end. Oversized positive offsets miss in forward search and clamp to the end in reverse search.

Both searches compare decoded characters: each invalid UTF-8 byte becomes a replacement character for matching, without modifying either input. An empty substring matches at the effective offset.

```vibe
text = "héllo hello"
[text.index("llo", 6), text.rindex("llo", 4), text.index("l", -3)]
# [8, 2, 8]
```

## Splitting

`split(separator: string? = nil, limit: int = 0) -> array<string>` returns an array of strings.

| Separator | Behavior |
| --- | --- |
| Omitted, `nil` or `" "` | Split runs of ASCII whitespace and discard leading whitespace. |
| Empty string | Split character byte windows; each invalid UTF-8 byte is one window. |
| Another string | Split exact, nonoverlapping byte sequences. |

A positive limit bounds the number of fields, leaving the remainder in the final field. Limit 1 returns the whole nonempty input, including whitespace. Limit 0 discards trailing empty fields; a negative limit preserves them. Empty input returns an empty array.

```vibe
[" a b  ".split(nil, 2), "a,b,".split(",", -1), "é🙂".split("")]
# [["a", "b  "], ["a", "b", ""], ["é", "🙂"]]
```

## Resource limits

Concatenation and splitting project complete result storage before allocating output. Split fields copy proper byte windows so a small retained field does not retain a large input; a whole-input field reuses its source. Discarded trailing fields allocate no output strings. The call's memory limit bounds output and search scratch.

Substring search stores the needle's decoded characters and search table, then streams the subject. Searches for `split`, `partition`, `rpartition`, `index`, `rindex`, `include?` and string-pattern `sub`, `gsub` and `scan` charge the bytes they read and their table transitions as bulk byte work, one step per 64 units, like copies. Repetition with `*` copies by doubling what it has written and is charged per byte copied. It does not construct a normalized copy of the subject. Symbol conversion shares immutable bytes within the call; importing the result into another call receives an independent memory charge.

Numeric parsing, comparisons, searching, projection and copies observe step limits, cancellation and deadlines. Exhaustion remains latched, and temporary storage is released on failure.
