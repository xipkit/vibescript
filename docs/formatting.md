# Percent formatting

`format(pattern: string, *values: array<any>) -> string` and the string `%` operator support indexed operands, flags, fixed widths and precisions, Unicode string precision, quoted and hexadecimal bytes, arbitrary integers, floats and malformed-directive diagnostics. The operator expands an array on its right into operands. Compound `%=` assignments use the same renderer and the existing addressed-write rules. Dynamic width and precision are rejected by the language parser for format strings.

`%.1f` renders one digit after the decimal point, including a trailing zero:

```vibe run
assert format("%.1f%%", 75.0) == "75.0%"
assert format("%.1f", 3.14159) == "3.1"
```

`sprintf` is a removed spelling of `format` (ADR-008). `format` validates keywords, blocks and the pattern argument first. It then calls eligible direct instances' `to_s` methods once per operand, in order, before inspecting the format string. Every operand is converted, including unused operands. Nested instances and operands to the string `%` operator retain their ordinary value rendering. Non-string conversion results also retain that rendering. Converted values stay accounted for throughout later callbacks and rendering; normal VM unwinding handles failures and cancellation.

Width, precision and projected output have fixed 1 MiB guards. These guards are conservative: they apply even when actual output would be smaller. Actual output is measured before allocating its final buffer, including extra text from malformed directives. The normalized pattern, prepared argument list, aggregate conversions, integer conversion scratch, quoted text and output buffer use the invocation's memory budget. Small numeric rendering uses fixed stack storage; large requested float precisions emit accounted padding beyond the finite binary float's exact decimal expansion. Cooperative work checks cover scanning, conversions and padding. Fixed guard failures are recoverable `LimitError` values; exhausted work, memory and cancellation remain latched.

Separate [boundary cases](formatting-boundaries.json) cover output limits, precision on shared collections and pointer syntax. They run with a larger work budget so script construction of multi-megabyte input does not hide the formatter's fixed guard. Native tests separately exercise finite memory and work quotas, retention across reentrant conversions, cancellation, helper-value restrictions and fresh-call cleanup.

Nested protected values and duplicates keep their own rendering, and `format("%.2s", 10**30)` keeps the requested prefix, `"10"`.

`%T` renders runtime type labels such as `int64`, `float64` and `*big.Int`. `%p` on a big integer renders its address, so pointer values differ across processes; separate cases verify their syntax, flags and padding.

## CSV quoting

There is no CSV helper. For a CSV field, double embedded quotes and surround
the field with quotes; always quoting also preserves commas and newlines.
Join fields with commas and rows with `"\r\n"`:

```vibe run
def csv_cell(text: string) -> string
  '"' + text.gsub('"', '""') + '"'
end
row = ["a,b", 'say "hi"', "two\nlines"].map { |cell| csv_cell(cell) }.join(",")
assert row == "\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\""
```

## Time and duration output

`Duration#iso8601` expresses elapsed time with ISO duration components:

```vibe run
assert Duration.parse("1m30s").iso8601 == "PT1M30S"
assert 0.seconds.iso8601 == "PT0S"
```

`Time#iso8601` uses a trailing `Z` in UTC. Convert with `.utc` before rendering
an input that may have another offset:

```vibe run
stamp = Time.parse("2026-09-01T14:30:00+02:00").utc
assert stamp.iso8601 == "2026-09-01T12:30:00Z"
```

`Time#strftime` supports these percent directives (names are in English):

| Directives | Output |
| --- | --- |
| `%Y`, `%C`, `%y` | Year, century, two-digit year |
| `%m`, `%B`, `%b`, `%h` | Month number, full name, abbreviated name (`%h` is `%b`) |
| `%d`, `%e`, `%j` | Day of month, space-padded day, day of year |
| `%A`, `%a`, `%w`, `%u` | Weekday name, abbreviation, Sunday-based number (0–6), Monday-based number (1–7) |
| `%H`, `%k`, `%I`, `%l` | 24-hour or 12-hour clock; `%k` and `%l` use space padding |
| `%M`, `%S`, `%L`, `%N` | Minute, second, millisecond, nanosecond |
| `%p`, `%P` | `AM`/`PM`, or lowercase |
| `%z`, `%:z`, `%::z`, `%:::z`, `%Z` | Offset as `+hhmm`, `+hh:mm`, `+hh:mm:ss`, shortest colon form, zone name |
| `%s` | Unix seconds |
| `%F`, `%D`, `%x` | `%Y-%m-%d`, or `%m/%d/%y` for both `%D` and `%x` |
| `%T`, `%X`, `%R`, `%r` | `%H:%M:%S` (both `%T` and `%X`), `%H:%M`, `%I:%M:%S %p` |
| `%c` | `%a %b %e %T %Y` |
| `%%`, `%n`, `%t` | Percent, newline, tab |

Flags `-`, `_`, `0`, `^` and `#` control padding and case; a decimal width
sets padding, or the number of fractional digits for `%L` and `%N`.
Unsupported directives are copied literally. A trailing bare `%` raises.

```vibe run
stamp = Time.parse("2026-09-01T12:30:45.123456789Z").utc
assert stamp.strftime("%Y-%m-%d %H:%M:%S %Z") == "2026-09-01 12:30:45 UTC"
assert stamp.strftime("%A %a %B %b %h %C %y %e %j %w %u") == "Tuesday Tue September Sep Sep 20 26  1 244 2 2"
assert stamp.strftime("%k %I %l %p %P %L %N") == "12 12 12 PM pm 123 123456789"
assert stamp.strftime("%z %:z %::z %:::z") == "+0000 +00:00 +00:00:00 +00"
assert stamp.strftime("%F %D %x %T %X %R %r") == "2026-09-01 09/01/26 09/01/26 12:30:45 12:30:45 12:30 12:30:45 PM"
assert stamp.strftime("%c %% %n%t") == "Tue Sep  1 12:30:45 2026 % \n\t"
assert Time.parse("1970-01-01T00:00:00Z").strftime("%s") == "0"
```
