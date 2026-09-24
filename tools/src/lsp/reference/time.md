# Time

Vibescript provides a `Time` object for working with instants in time, plus helpers on `Duration` for producing `Time` values (`ago`, `after`, `since`, etc.). Method declarations omit parentheses when there are no arguments:

```vibe
def midnight_utc
  Time.utc(2024, 1, 1)
end
```

## Creating Time values

- `Time.new(year, month=1, day=1, hour=0, min=0, sec=0, zone=nil, in: zone)`
- `Time.local(year, month=1, day=1, hour=0, min=0, sec=0, usec=0)` / `Time.mktime(...)`
- `Time.utc(year, month=1, day=1, hour=0, min=0, sec=0, usec=0)` / `Time.gm(...)`
- `Time.at(seconds_since_epoch, subsec=nil, unit=nil, in: zone)`
- `Time.now(in: zone)`
- `Time.parse(string, layout=nil, in: zone)`

Only the year is required for the calendar constructors. As in Ruby, an omitted month or day defaults to `1` (so `Time.utc(2024)` is January 1, 2024 and `Time.utc(2024, 2)` is February 1, 2024) and omitted time fields default to midnight. An explicit `nil` in an optional position is treated the same as omitting it, so `Time.utc(2024, nil)` is also January 1, 2024.

```vibe
def start_of_year
  Time.utc(2024).iso8601 # "2024-01-01T00:00:00Z"
end
```

Zones accept Go-style names (e.g. `"America/New_York"`), `"UTC"`/`"GMT"`, `"LOCAL"`, or numeric offsets like `"+05:30"`.

The seventh positional argument differs by constructor, matching Ruby. For `Time.new` it is a zone/offset (the location may also be set with `in:`). For `Time.local`/`Time.mktime`/`Time.utc`/`Time.gm` it is microseconds-with-fraction; the location is fixed by the constructor (local for `local`/`mktime`, UTC for `utc`/`gm`). Integer microseconds are exact and floats carry sub-microsecond precision down to the nanosecond. A non-numeric microsecond argument raises a runtime error.

```vibe
def with_microseconds
  Time.utc(2024, 1, 2, 3, 4, 5, 123456).nsec # 123456000
end
```

`Time.at` accepts Ruby-style subsecond arguments. The first argument is epoch seconds (integer or float, with floats carrying their fraction). An optional second positional argument adds a subsecond offset that defaults to microseconds, and an optional third positional symbol selects the unit: `:microsecond`/`:usec`, `:millisecond`, or `:nanosecond`/`:nsec`. A unit without a subsecond value, an unknown unit symbol, or a non-numeric subsecond value raises a runtime error. Unlike the calendar constructors (`Time.utc`/`Time.local`), `Time.at` does not treat an explicit `nil` subsecond as omitted: `Time.at(0, nil)` raises just as Ruby does. The `in:` zone keyword composes with every form. Subsecond values are backed by nanosecond-resolution timestamps, so a fractional nanosecond is floored toward negative infinity the way Ruby exposes it: `Time.at(0, -1.9, :nsec).nsec == 999999998`. A subsecond value larger than one second carries into the seconds (and a negative value borrows from them), matching Ruby; a magnitude too large to express within the nanosecond range raises `Time.at subsecond value out of range` rather than wrapping into a bogus instant.

```vibe
def from_epoch
  {
    float:       Time.at(0.123456).utc.nsec,                  # 123456000
    micro:       Time.at(0, 123456).utc.nsec,                 # 123456000
    micro_unit:  Time.at(0, 123456, :microsecond).utc.nsec,  # 123456000
    milli:       Time.at(0, 123, :millisecond).utc.nsec,      # 123000000
    nano:        Time.at(0, 123456789, :nsec).utc.nsec,       # 123456789
    zoned:       Time.at(0, 123456, in: "+05:30").utc_offset  # 19800
  }
end
```

Without an explicit `layout`, `Time.parse` accepts common formats such as RFC3339/RFC1123, `YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY-MM-DD HH:MM:SS`, and `MM/DD/YYYY` (with optional time).

A timestamp that carries no zone is read as UTC, and `Time.now` returns UTC, so
a script produces the same result on every host regardless of its `TZ`. Pass
`in:` to work in a particular zone (`Time.now(in: "America/New_York")`,
`Time.parse("2026-07-27", in: "Asia/Tokyo")`); an explicit `Z` or offset in the
input is always honored as written.

An input naming a zone *abbreviation* (`Mon, 27 Jul 2026 14:30:45 EDT`) is not
zoneless, so it resolves against the host's zone database rather than the UTC
default -- an abbreviation cannot be interpreted without one. Use an explicit
offset (`-0400`) when the result must not depend on the host.

## Formatting

`Time#format` uses Go layouts, built from the reference time `Mon Jan 2 15:04:05 MST 2006`:

```vibe
def formatted_timestamp
  t = Time.utc(2000, 1, 1, 20, 15, 1)
  {
    short_date: t.format("2006-01-02"),
    long_time: t.format("15:04:05"),
    rfc3339:   t.format("2006-01-02T15:04:05Z07:00")
  }
end
```

`Time#strftime` takes a Ruby-style percent format string, so Ruby formatting code runs unchanged:

The two methods take different, incompatible format languages, and crossing
them is an error rather than a silent passthrough: `t.strftime("2006-01-02")`
and `t.format("%Y-%m-%d")` both report which language they expected and which
method takes the one you supplied.


```vibe
def strftime_timestamp
  t = Time.utc(2024, 1, 2, 3, 4, 5)
  {
    iso:      t.strftime("%Y-%m-%d %H:%M:%S"), # "2024-01-02 03:04:05"
    weekday:  t.strftime("%A, %B %e"),         # "Tuesday, January  2"
    clock:    t.strftime("%I:%M %p"),          # "03:04 AM"
    escaped:  t.strftime("100%% done")         # "100% done"
  }
end
```

Supported directives cover the common subset:

| Directive | Meaning | Example |
| --- | --- | --- |
| `%Y` `%C` `%y` | Year (4+ digits), century, year-in-century | `2024` `20` `24` |
| `%m` `%d` `%e` `%j` | Month, day (zero/blank padded), day of year | `01` `02` ` 2` `002` |
| `%H` `%k` `%I` `%l` | Hour 24h/12h (zero/blank padded) | `03` ` 3` `03` ` 3` |
| `%M` `%S` `%L` `%N` | Minute, second, milli/nanoseconds | `04` `05` `123` `123456789` |
| `%p` `%P` | Meridian upper/lower | `AM` `am` |
| `%A` `%a` `%B` `%b`/`%h` | Weekday/month names (English) | `Tuesday` `Tue` `January` `Jan` |
| `%w` `%u` | Weekday number (Sunday 0 / Monday 1) | `2` `2` |
| `%s` `%z` `%:z` `%::z` `%Z` | Epoch seconds, UTC offset, zone name | `1704164645` `+0530` `+05:30` `+05:30:00` `UTC` |
| `%n` `%t` `%%` | Newline, tab, literal percent | `\n` `\t` `%` |
| `%F` `%T`/`%X` `%R` `%D`/`%x` `%r` `%c` | Compound date/time shortcuts | `2024-01-02` `03:04:05` `03:04` `01/02/24` `03:04:05 AM` `Tue Jan  2 03:04:05 2024` |

Directives accept Ruby's optional flags and width between the `%` and the directive letter, written as `%<flags><width><letter>`:

| Flag | Effect | Example |
| --- | --- | --- |
| `-` | Omit padding | `%-d` → `2` |
| `_` | Pad with spaces | `%_d` → ` 2` |
| `0` | Pad with zeros (even for normally blank-padded directives) | `%0e` → `02` |
| `^` | Uppercase the result | `%^B` → `JANUARY` |
| `#` | Toggle case: lowercase an all-uppercase result, otherwise uppercase it | `%#p` → `am`, `%#B` → `JANUARY` |

The width sets a minimum field width and applies to **every** numeric and name directive, not just `%N`:

```vibe
def strftime_flags
  t = Time.utc(2024, 1, 2, 3, 4, 5)
  {
    day_no_pad:   t.strftime("%-d"),  # "2"
    day_padded:   t.strftime("%03d"), # "002"
    year_wide:    t.strftime("%6Y"),  # "002024"
    month_upper:  t.strftime("%^B"),  # "JANUARY"
    meridian_low: t.strftime("%#p")   # "am"
  }
end
```

`%N` and `%L` interpret the width as the number of fractional-second digits (`%3N`, `%6N`, `%9N`, …), truncating beyond nanosecond resolution and zero-padding past it. Compound directives expand a fixed sub-format: the `^` flag propagates into the expansion (`%^c` uppercases the nested names) while `#` does not, and a width pads the whole expansion as one field (`%12F` → `  2024-01-02`). The `%z` offset honors width with zero padding (`%6z` → `+00530`) but treats the `_` space-padding flag as a no-op, keeping the offset intact rather than reproducing Ruby's lossy space-padded form.

`%Z` mirrors `Time#zone`, so a fixed-offset receiver renders its offset name (`+05:30`) rather than Ruby's empty string. Unknown directives are passed through verbatim (`%Q` stays `%Q`), matching Ruby, but a format whose trailing `%` has no directive (or a modifier such as `%6`/`%:` with no directive) raises a runtime error.

`Time#to_s` uses RFC3339Nano. `Time#iso8601`, `Time#xmlschema`, and `Time#rfc3339` return RFC3339, defaulting to whole-second precision. `xmlschema` is an alias for `iso8601`.

They accept an optional `ndigits` argument (matching Ruby's `Time#iso8601(ndigits = 0)`) to append fractional-second digits. Fractional seconds are truncated toward zero, and requesting more digits than the nanosecond clock can resolve zero-pads the remainder:

```vibe
def fractional_timestamps
  t = Time.parse("1970-01-01T00:00:00.123456Z")
  {
    seconds:      t.iso8601,     # "1970-01-01T00:00:00Z"
    milliseconds: t.iso8601(3),  # "1970-01-01T00:00:00.123Z"
    microseconds: t.iso8601(6),  # "1970-01-01T00:00:00.123456Z"
    padded:       t.iso8601(12)  # "1970-01-01T00:00:00.123456000000Z"
  }
end
```

A negative `ndigits`, a non-integer argument, more than one argument, or a precision above 100 digits raises a runtime error.

`Time#httpdate` renders the HTTP-date / IMF-fixdate form (RFC 7231), always in GMT. `Time#rfc2822` and its alias `Time#rfc822` render the RFC 2822 mail date, preserving the receiver's zone offset; a genuine UTC receiver uses the `-0000` zone Ruby reserves for timestamps without real zone information, while an explicit zero offset uses `+0000`. Both helpers drop sub-second precision because their grammars have whole-second resolution and take no arguments:

```vibe
def mail_and_http_dates
  utc = Time.utc(2024, 1, 2, 3, 4, 5)
  offset = Time.parse("2024-01-02 03:04:05", "2006-01-02 15:04:05", in: "+05:30")
  {
    httpdate:     utc.httpdate,     # "Tue, 02 Jan 2024 03:04:05 GMT"
    rfc2822:      utc.rfc2822,      # "Tue, 02 Jan 2024 03:04:05 -0000"
    rfc822:       utc.rfc822,       # "Tue, 02 Jan 2024 03:04:05 -0000"
    http_offset:  offset.httpdate,  # "Mon, 01 Jan 2024 21:34:05 GMT"
    mail_offset:  offset.rfc2822    # "Tue, 02 Jan 2024 03:04:05 +0530"
  }
end
```

## Accessors and predicates

- Date/time parts: `year`, `month`/`mon`, `day`/`mday`, `hour`, `min`, `sec`, `usec`/`tv_usec`, `nsec`/`tv_nsec`, `subsec`
- Day offsets: `wday`, `yday`
- Zone/offset: `zone`, `utc_offset`/`gmt_offset`/`gmtoff`
- Flags: `utc?`, `dst?`, `sunday?`, `monday?`, ... through `saturday?`

## Conversions

- Epoch: `to_i`/`tv_sec`, `to_f`, `to_r`
- Zone conversion: `getutc`/`getgm`, `utc`/`gmtime`, and `getlocal(offset = nil)`/`localtime(offset = nil)`. With no argument the latter two convert to the host's local zone; passing a zone such as `"+05:30"`, `"-04:00"`, `"America/New_York"`, or `"UTC"` returns the same instant in that zone. They always return a new `Time` rather than mutating the receiver.
- String: `to_s` (RFC3339Nano), `format(layout)` (Go layout), `strftime(format)` (Ruby percent format)
- RFC3339 aliases: `iso8601(ndigits = 0)`, `xmlschema(ndigits = 0)`, `rfc3339(ndigits = 0)` (optional fractional-second precision)
- HTTP/mail dates: `httpdate` (IMF-fixdate, always GMT), `rfc2822`/`rfc822` (RFC 2822 mail date, preserving the receiver's offset)
- Tuple: `to_a` returns `[sec, min, hour, mday, month, year, wday, yday, isdst, zone]`, matching Ruby's positional field order. Field values reuse the individual accessors, so the result reflects the receiver's UTC/local/offset zone.

## Comparisons and math

- Compare: `<=>`, `eql?`. `<=>` returns `-1`/`0`/`1` for two `Time` values and `nil` when the other operand is not a `Time`, matching Ruby's spaceship contract; it raises only when given the wrong number of arguments. `eql?` is a predicate: it returns `true` only when both operands are equal `Time` values, returns `false` for an unequal `Time` or a non-`Time` operand (matching Ruby's `Time#eql?`), and raises only when given the wrong number of arguments. Like every value, a `Time` also answers the universal `equal?` identity predicate, which matches `eql?` for an immutable time; see [Universal Predicates](stdlib_core_utilities.md#universal-predicates).
- Add/sub durations: `time + duration`, `time - duration`
- Add/sub seconds: `time + number`, `time - number`, where the number is interpreted as seconds (matching Ruby). Integers shift by whole seconds; floats carry sub-second precision down to the nanosecond, and negative values shift backward. The result is a new `Time`. Numeric addition commutes (`number + time`), but subtracting a `Time` from a number is undefined, just as in Ruby.
- Difference of times: `time - time` → a `Float` number of seconds (matching Ruby's `Time#-`), preserving sub-second precision

```vibe
def time_seconds_math
  t = Time.utc(2024, 1, 1, 0, 0, 0)
  {
    one_minute_later:   (t + 60).iso8601,    # "2024-01-01T00:01:00Z"
    one_minute_earlier: (t - 60).iso8601,    # "2023-12-31T23:59:00Z"
    half_second_later:  (t + 0.5).iso8601(3),# "2024-01-01T00:00:00.500Z"
    span_seconds:       (t + 90) - t         # 90.0
  }
end
```

An out-of-range numeric offset (including a non-finite float such as `Infinity` or `NaN`) raises a runtime error.

## Duration shortcuts that yield Time

Durations can move times forward/backward:

```vibe
def five_minutes_ago
  5.minutes.ago(Time.now)
end

def in_two_hours
  2.hours.after(Time.now)
end

```
