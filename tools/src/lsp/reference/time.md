# Time

A `time` value is an instant with a zone. The `Time` namespace builds and
parses times, durations anchor them (`5.minutes.ago`), and every conversion
returns a new time; a time never changes in place.

```vibe
def midnight_utc -> time
  Time.utc(2024, 1, 1)
end

midnight_utc.iso8601  # "2024-01-01T00:00:00Z"
```

## Creating times

- `Time.utc(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0) -> time`
  – calendar parts in UTC.
- `Time.local(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0, *, in: string? = nil) -> time`
  – calendar parts in the zone `in:` names, or in the host's local zone.
- `Time.at(seconds: number, subsec?: number, unit?: :microsecond | :millisecond | :nanosecond, *, in: string? = nil) -> time`
  – Unix epoch seconds, in the zone `in:` names or the host's local zone.
- `Time.now(*, in: string? = nil) -> time` – the current time, in UTC unless
  `in:` names a zone.
- `Time.parse(text: string, layout: string? = nil, *, in: string? = nil) -> time`
  – parses common formats, or `text` in a Go layout.

Only the year is required by the calendar constructors: an omitted month or
day is `1` and omitted time fields are midnight, so `Time.utc(2024)` is January
1, 2024 and `Time.utc(2024, 2)` is February 1. The seventh argument is
microseconds; an integer is exact and a float carries its fraction down to the
nanosecond.

```vibe
Time.utc(2024).iso8601                            # "2024-01-01T00:00:00Z"
Time.utc(2024, 2).iso8601                         # "2024-02-01T00:00:00Z"
Time.utc(2024, 1, 2, 3, 4, 5, 123456).nsec        # 123456000
Time.local(2024, 1, 2, in: "Asia/Tokyo").iso8601  # "2024-01-02T00:00:00+09:00"
```

`Time.at` takes epoch seconds as an int or a float. An optional second
argument adds a subsecond offset in microseconds, and an optional third
selects its unit: `:microsecond`, `:millisecond` or `:nanosecond`. A fractional
nanosecond is floored, a subsecond value larger than a second carries into the
seconds, and one too large for the nanosecond range raises `Time.at subsecond
value out of range`.

```vibe
Time.at(0.123456).utc.nsec                  # 123456000
Time.at(0, 123456).utc.nsec                 # 123456000
Time.at(0, 123, :millisecond).utc.nsec      # 123000000
Time.at(0, 123456789, :nanosecond).utc.nsec # 123456789
Time.at(0, in: "+05:30").utc_offset         # 19800
```

Without a layout, `Time.parse` accepts RFC 3339 and RFC 1123 timestamps,
`YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY-MM-DD HH:MM:SS` and `MM/DD/YYYY`, each with an
optional time. A layout is a Go reference-time layout, as `format` takes.

```vibe
Time.parse("2024-01-02").iso8601  # "2024-01-02T00:00:00Z"
tokyo = Time.parse("2024-01-02 03:04", "2006-01-02 15:04", in: "Asia/Tokyo")
tokyo.iso8601                     # "2024-01-02T03:04:00+09:00"
```

## Zones

Zones are IANA names such as `"America/New_York"`, `"UTC"` or `"GMT"`,
`"LOCAL"` for the host's zone, or numeric offsets such as `"+05:30"`.

A timestamp without a zone is read as UTC, and `Time.now` is UTC, so a script
gives the same result on every host whatever its `TZ`. Pass `in:` to work in a
zone (`Time.now(in: "America/New_York")`, `Time.parse("2026-07-27", in:
"Asia/Tokyo")`); an explicit `Z` or offset in the input is always honoured.
`Time.local` and `Time.at` without `in:` use the host's zone.

An input that names a zone abbreviation, such as `Mon, 27 Jul 2026 14:30:45
EDT`, resolves the abbreviation against the host's zone database. Write an
explicit offset (`-0400`) when the result must not depend on the host.

### `utc -> time`

The same instant in UTC.

### `localtime(zone: string? = nil) -> time`

The same instant in `zone`, or in the host's zone when it is omitted or `nil`.

```vibe
t = Time.utc(2024, 1, 2, 3, 4, 5)
t.localtime("+05:30").iso8601  # "2024-01-02T08:34:05+05:30"
t.localtime("+05:30").zone     # "+05:30"
```

- `zone -> string` – the zone's name or abbreviation, such as `"UTC"`, or the
  offset of a fixed-offset time, such as `"+05:30"`.
- `utc_offset -> int` – the offset from UTC in seconds.
- `utc? -> bool` – whether the time is in UTC.
- `dst? -> bool` – whether daylight saving time is in effect.

## Formatting

### `format(layout: string) -> string`

Formats with a Go layout, written as the reference time
`Mon Jan 2 15:04:05 MST 2006`.

```vibe
t = Time.utc(2000, 1, 1, 20, 15, 1)
t.format("2006-01-02")                 # "2000-01-01"
t.format("15:04:05")                   # "20:15:01"
t.format("2006-01-02T15:04:05Z07:00")  # "2000-01-01T20:15:01Z"
```

### `strftime(format: string) -> string`

Formats with a percent pattern such as `"%Y-%m-%d"`. The two formatters take
different languages, and crossing them raises: `t.strftime("2006-01-02")` and
`t.format("%Y-%m-%d")` each name the method that takes the other language.

```vibe
t = Time.utc(2024, 1, 2, 3, 4, 5)
t.strftime("%Y-%m-%d %H:%M:%S")  # "2024-01-02 03:04:05"
t.strftime("%A, %B %e")          # "Tuesday, January  2"
t.strftime("%I:%M %p")           # "03:04 AM"
t.strftime("100%% done")         # "100% done"
```

| Directive | Meaning | Example |
| --- | --- | --- |
| `%Y` `%C` `%y` | Year (4+ digits), century, year in century | `2024` `20` `24` |
| `%m` `%d` `%e` `%j` | Month, day (zero or blank padded), day of year | `01` `02` ` 2` `002` |
| `%H` `%k` `%I` `%l` | Hour, 24h and 12h (zero or blank padded) | `03` ` 3` `03` ` 3` |
| `%M` `%S` `%L` `%N` | Minute, second, milliseconds, nanoseconds | `04` `05` `123` `123456789` |
| `%p` `%P` | Meridian, upper and lower case | `AM` `am` |
| `%A` `%a` `%B` `%b` `%h` | Weekday and month names (English) | `Tuesday` `Tue` `January` `Jan` `Jan` |
| `%w` `%u` | Weekday number (Sunday 0, Monday 1) | `2` `2` |
| `%s` `%z` `%:z` `%::z` `%Z` | Epoch seconds, UTC offset, zone name | `1704164645` `+0530` `+05:30` `+05:30:00` `UTC` |
| `%n` `%t` `%%` | Newline, tab, literal percent | `\n` `\t` `%` |
| `%F` `%T` `%X` `%R` `%D` `%x` `%r` `%c` | Compound shortcuts | `2024-01-02` `03:04:05` `03:04:05` `03:04` `01/02/24` `01/02/24` `03:04:05 AM` `Tue Jan  2 03:04:05 2024` |

Flags and a width go between the `%` and the directive, as
`%<flags><width><letter>`:

| Flag | Effect | Example |
| --- | --- | --- |
| `-` | Omit padding | `%-d` → `2` |
| `_` | Pad with spaces | `%_d` → ` 2` |
| `0` | Pad with zeros, even for blank-padded directives | `%0e` → `02` |
| `^` | Uppercase the result | `%^B` → `JANUARY` |
| `#` | Lowercase an all-uppercase result, otherwise uppercase it | `%#p` → `am`, `%#B` → `JANUARY` |

```vibe
t = Time.utc(2024, 1, 2, 3, 4, 5)
t.strftime("%-d")   # "2"
t.strftime("%03d")  # "002"
t.strftime("%6Y")   # "002024"
t.strftime("%^B")   # "JANUARY"
t.strftime("%#p")   # "am"
```

The width is a minimum field width for every numeric and name directive. For
`%N` and `%L` it is the number of fractional digits (`%3N`, `%6N`, `%9N`),
truncating below nanoseconds and zero-padding past them. A compound directive
expands a fixed pattern: `^` applies to the names inside it (`%^c`) and `#`
does not, and a width pads the whole expansion (`%12F` → `  2024-01-02`). `%z`
pads its offset with zeros (`%6z` → `+00530`) and ignores `_`. `%Z` is the
time's `zone`, so a fixed-offset time renders its offset. An unknown directive
is copied as written (`%Q` stays `%Q`); a trailing `%`, or a flag or width
without a directive, raises. Output is capped at 1 MiB.

### `iso8601(digits: int = 0) -> string`

The RFC 3339 form, in whole seconds unless `digits` asks for fractional
digits. The fraction is truncated, and digits past nanoseconds are zeros. A
negative `digits`, or more than 100, raises.

```vibe
t = Time.parse("1970-01-01T00:00:00.123456Z")
t.iso8601      # "1970-01-01T00:00:00Z"
t.iso8601(3)   # "1970-01-01T00:00:00.123Z"
t.iso8601(6)   # "1970-01-01T00:00:00.123456Z"
t.iso8601(12)  # "1970-01-01T00:00:00.123456000000Z"
```

### `to_s -> string`

The RFC 3339 form with as many fractional digits as the time has, up to
nanoseconds, as string interpolation renders it.

### `httpdate -> string`

The HTTP date (RFC 7231 IMF-fixdate), always in GMT, in whole seconds.

### `rfc2822 -> string`

The RFC 2822 mail date in the time's own offset, in whole seconds. A UTC time
uses the zone `-0000`, which marks a timestamp without local zone information;
an explicit zero offset uses `+0000`.

```vibe
utc = Time.utc(2024, 1, 2, 3, 4, 5)
offset = Time.parse("2024-01-02 03:04:05", "2006-01-02 15:04:05", in: "+05:30")
utc.httpdate      # "Tue, 02 Jan 2024 03:04:05 GMT"
utc.rfc2822       # "Tue, 02 Jan 2024 03:04:05 -0000"
offset.httpdate   # "Mon, 01 Jan 2024 21:34:05 GMT"
offset.rfc2822    # "Tue, 02 Jan 2024 03:04:05 +0530"
```

## Components

- `year -> int` – the calendar year.
- `month -> int` – the month, 1 to 12.
- `day -> int` – the day of the month.
- `hour -> int` – the hour, 0 to 23.
- `min -> int` – the minute.
- `sec -> int` – the second.
- `usec -> int` – the microseconds within the second.
- `nsec -> int` – the nanoseconds within the second.
- `subsec -> float` – the fraction of the second.
- `wday -> int` – the day of the week, 0 for Sunday.
- `yday -> int` – the day of the year, 1 to 366.

Components read the time in its own zone.

```vibe
t = Time.utc(2024, 1, 2, 3, 4, 5)
t.year   # 2024
t.wday   # 2
t.yday   # 2
```

## Weekdays

- `sunday? -> bool` – whether the time falls on a Sunday.
- `monday? -> bool` – whether the time falls on a Monday.
- `tuesday? -> bool` – whether the time falls on a Tuesday.
- `wednesday? -> bool` – whether the time falls on a Wednesday.
- `thursday? -> bool` – whether the time falls on a Thursday.
- `friday? -> bool` – whether the time falls on a Friday.
- `saturday? -> bool` – whether the time falls on a Saturday.

## Conversions

- `to_i -> int` – whole seconds since the Unix epoch.
- `to_f -> float` – seconds since the Unix epoch, with the fraction.

### `to_a -> [int, int, int, int, int, int, int, int, bool, string]`

The fields `[sec, min, hour, day, month, year, wday, yday, dst?, zone]`, read in
the time's zone.

```vibe
Time.utc(2024, 1, 2, 3, 4, 5).to_a  # [5, 4, 3, 2, 1, 2024, 2, 2, false, "UTC"]
```

## Comparison and arithmetic

Times order with `<`, `<=`, `>`, `>=` and `<=>`, compare with `==`, and sort.

- `time + duration` and `time - duration` shift by a duration.
- `time + number` and `time - number` shift by seconds; a float carries its
  fraction down to the nanosecond. An out-of-range or non-finite number raises.
- `time - time` is the difference in seconds, as a `float`.

```vibe
t = Time.utc(2024, 1, 1)
(t + 60).iso8601         # "2024-01-01T00:01:00Z"
(t - 60).iso8601         # "2023-12-31T23:59:00Z"
(t + 0.5).iso8601(3)     # "2024-01-01T00:00:00.500Z"
(t + 90) - t             # 90.0
(t + 5.minutes).iso8601  # "2024-01-01T00:05:00Z"
```

- `between?(min: time, max: time) -> bool` – whether the time is within the
  bounds, inclusive.

### `round(digits: int = 0) -> time`

Rounds to `digits` fractional-second digits, halves away from zero; `0` rounds
to whole seconds, and digits past nanoseconds change nothing. A negative
`digits` raises.

- `floor -> time` – truncates to the whole second.
- `ceil -> time` – rounds up to the next whole second.

## Durations that give times

A duration counts from now with `ago` and `from_now`, or from a given time
with `before` and `after`. See [durations](durations.md).

```vibe
start = Time.utc(2024, 1, 15, 10)
30.minutes.before(start).iso8601  # "2024-01-15T09:30:00Z"
2.hours.after(start).iso8601      # "2024-01-15T12:00:00Z"
reminder = 5.minutes.ago          # five minutes before now, in UTC
```

## Removed spellings

These names still run until the static language is the default, but do not
compile with static types. Each has one replacement.

- `mon` – removed; use `month`.
- `mday` – removed; use `day`.
- `tv_sec` – removed; use `to_i`.
- `tv_usec` – removed; use `usec`.
- `tv_nsec` – removed; use `nsec`.
- `to_r` – removed; use `to_f`.
- `hash` – removed; `to_i * 1000000000 + nsec` identifies the instant.
- `gmt_offset`, `gmtoff` – removed; use `utc_offset`.
- `gmtime`, `getutc`, `getgm` – removed; use `utc`.
- `getlocal` – removed; use `localtime`.
- `gmt?` – removed; use `utc?`.
- `isdst` – removed; use `dst?`.
- `xmlschema`, `rfc3339` – removed; use `iso8601`.
- `rfc822` – removed; use `rfc2822`.
- `string` – removed; use `to_s`, the time's RFC 3339 form.
- `Time.gm` – removed; use `Time.utc`.
- `Time.mktime` – removed; use `Time.local`.
- `Time.new` – removed; use `Time.local`, passing a zone as `in:`.
