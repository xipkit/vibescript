# String iteration

`lines` returns an array split at newline bytes, retaining each newline. A final newline adds no empty trailing element, an empty string produces an empty array, and carriage returns remain unchanged.

```vibe
"a\r\nb\n".lines
```

The result is `["a\r\n", "b\n"]`.

The streaming methods require a synchronous block and take no positional or keyword arguments, including on empty strings. Each callback receives one value:

| Method | Yielded value |
| --- | --- |
| `each_byte` | Raw byte as an integer from 0 through 255 |
| `each_char` | One Unicode code point as a string |
| `each_codepoint` | Unicode code point as an integer |
| `each_line` | One line with its trailing newline, if present |

```vibe
out: array<int> = []
"é".each_byte { |byte| out.push(byte) }
out
```

The result is `[195, 169]`. Rune iteration decodes valid UTF-8 and substitutes U+FFFD for each invalid byte. Byte and line iteration preserve the original bytes. Combining marks remain separate code points; `each_char` does not split by grapheme cluster.

```vibe
out: array<int> = []
"Aé🎉".each_codepoint { |point| out.push(point) }
out
```

The result is `[65, 233, 127881]`. Normal completion returns the original receiver and ignores each callback's result. Rebinding the source variable does not change the values still to be visited. `break`, `next` and `return` follow the language's block control-flow rules. The block parameter takes its type from the signature, such as `int` for `each_byte`.

```vibe
out: array<string> = []
"red\nblue\n".each_line { |line| out.push(line.chomp) }
out
```

The result is `["red", "blue"]`. Streaming holds one yielded value at a time unless the script retains earlier values. Scans and callback boundaries observe work, cancellation and deadline limits. Proper line windows copy their bytes so keeping a short line does not retain a large subject. A line spanning the whole input reuses that input's storage. `lines` preflights all output slots, string headers and copied bytes before allocating its result.

The materializing methods `lines`, `chars`, `bytes` and `codepoints` take no arguments or block. No form accepts a custom line separator or a `chomp:` keyword.
