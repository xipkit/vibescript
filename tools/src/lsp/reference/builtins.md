# Builtin functions

Every script can call these global functions and namespaces. Signatures are
written as in `vibes prelude`: `name?: T` is an optional argument, `*name` takes
any number of arguments, parameters after a bare `*` are keywords, and a call
without arguments takes no parentheses.

## Assertions

### `assert(condition: bool, message?: string)`

Raises an `AssertionError` when `condition` is false, with `message` or
`assertion failed`. Use it to check preconditions.

```vibe
def validate_amount(amount: int) -> int
  assert amount > 0, "amount must be positive"
  amount
end

validate_amount(5)  # 5
```

## Output

### `puts(*values: array<any>)`

Writes each value to the configured output, one per line, rendered as string
interpolation renders it; with no values it writes one blank line.

```vibe
puts "processing", 42
```

### `print(*values: array<any>)`

Writes each value to the configured output without a line break.

```vibe
print "loading", "...\n"  # loading...
```

### `p<T>(value: T) -> T` / `p` / `p(first: any, second: any, *rest: array<any>) -> array<any>`

Writes each value in inspect form, where strings keep their quotes, one per
line, and returns what it was given: the value for one argument, an array of
the values for several, and `nil` for none. Use it to print a value inside a
larger expression.

```vibe
count = p(42) + 1  # prints 42; count is 43
pair = p("id", 7)  # prints "id" and 7; pair is ["id", 7]
```

### `warn(*values: array<any>)`

Writes each value to the configured error output, one per line.

```vibe
warn "rate limit nearly reached"
```

## Formatting

### `format(pattern: string, *values: array<any>) -> string`

Formats values with a percent pattern: `%s`, `%d`, `%f`, `%x`, `%o`, `%b`,
`%e`, `%q` and `%%`, with flags, width and precision such as `%05d` or `%.2f`,
and indexed operands such as `%2$s`. `string % values` formats the same way.
Output is capped at 1 MiB before padding is built.

```vibe
format("%.2f", 1.234)          # "1.23"
format("%x", 255)              # "ff"
format("%2$s %1$s", "a", "b")  # "b a"
"%s:%03d" % ["id", 7]          # "id:007"
```

## Money

### `money(amount: string) -> money`

Parses an amount and a three-letter currency code, such as `"12.50 USD"`.

```vibe
total = money("100.50 USD")
fee = money("2.50 USD")
net = total - fee  # 98.00 USD
```

### `money_cents(cents: int, currency: string) -> money`

Builds money from a whole number of cents.

```vibe
price = money_cents(2550, "USD")  # 25.50 USD
price.cents                       # 2550
```

## Random values

### `uuid -> string`

A new RFC 9562 version 7 UUID.

```vibe
event_id = uuid
```

### `random_id(length: int = 16) -> string`

A random alphanumeric token of `length` characters, from 1 to 1,024.

```vibe
short = random_id(8)
token = random_id
```

### `rand(max: int | range) -> int` / `rand -> float`

A random float in `[0, 1)` without an argument, an int from `0` up to but not
including `max`, or an int inside an integer range.

```vibe
chance = rand       # a float below 1.0
die = rand(1..6)    # an int from 1 to 6
index = rand(10)    # an int from 0 to 9
```

### `srand(seed: int? = nil) -> int?`

Seeds this call's `rand` sequence and returns the previous seed, or `nil`
when there was none. Without a seed it picks a fresh random one. The same seed
gives the same sequence within a call, and a seed never carries into later
calls.

```vibe
srand(1234)
first = [rand(10), rand(10)]
srand(1234)
again = [rand(10), rand(10)]
first == again  # true
```

## Numeric conversion

### `to_int(value: number | string) -> int`

Converts an int, a float with no fractional part, or a base-10 integer
string. Anything else raises.

### `to_float(value: number | string) -> float`

Converts an int, a float, or a finite decimal or hexadecimal float string.
Anything else raises.

```vibe
count = to_int("42")      # 42
whole = to_int(3.0)       # 3
ratio = to_float("1.25")  # 1.25
```

## Control flow

### `loop(&block: ()) -> any`

Runs the block until it breaks. `break value` makes `value` the result, and
`next` starts the next pass.

```vibe
x = 0
result = loop {
  x += 1
  if x == 3
    break "done"
  end
}
```

## Module loading

### `require(path: string, *, as: string? = nil) -> any`

Loads a module from the configured module paths and returns a namespace of its
public functions, which are also bound by name in the requiring script when
the name is free. `def` and `export def` are public; `private def` stays in the
module. The module name and the `as:` alias are string literals, so the
compiler resolves the module and checks every call into it with its declared
types before the script runs. A module's top-level statements run once per
call, before its functions are returned.

```vibe module=fees.vibe
export def calculate_fee(amount: int) -> int
  amount // 20
end
```

```vibe
def calculate_total(amount: int) -> int
  require("fees", as: "helpers")
  amount + helpers.calculate_fee(amount)
end

calculate_total(100)  # 105
```

## Time

`Time` builds, reads and parses time values. Zones are IANA names such as
`"America/New_York"`, `"UTC"`, `"LOCAL"` for the host's zone, or offsets such
as `"+05:30"`. See [Time](time.md) for the members of a time.

### `Time.now(*, in: string? = nil) -> time`

The current time, in UTC unless `in:` names a zone.

### `Time.utc(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0) -> time`

A calendar time in UTC. Omitted fields default to January 1 at midnight.

### `Time.local(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0, *, in: string? = nil) -> time`

A calendar time in the zone `in:` names, or in the host's zone.

### `Time.at(seconds: number, subsec?: number, unit?: :microsecond | :millisecond | :nanosecond, *, in: string? = nil) -> time`

A time from Unix epoch seconds, with an optional subsecond offset in
microseconds or the given unit.

### `Time.parse(text: string, layout: string? = nil, *, in: string? = nil) -> time`

Parses RFC 3339, RFC 1123, `YYYY-MM-DD` and similar common formats, or `text`
in a Go layout such as `"2006-01-02"`. A timestamp without a zone is read in
UTC, or in the zone `in:` names.

```vibe
Time.utc(2024).iso8601                            # "2024-01-01T00:00:00Z"
Time.local(2024, 1, 2, in: "Asia/Tokyo").iso8601  # "2024-01-02T00:00:00+09:00"
Time.parse("2024-01-02").iso8601                  # "2024-01-02T00:00:00Z"
Time.at(0).utc.iso8601                            # "1970-01-01T00:00:00Z"
```

### Removed spellings

- `Time.gm` – removed; use `Time.utc`.
- `Time.mktime` – removed; use `Time.local`.
- `Time.new` – removed; use `Time.local`, passing a zone as `in:`.

## Duration

`Duration` builds duration values from parts and parses them from text.
Durations usually come from integer units such as `5.minutes`; see
[Durations](durations.md) for their members.

### `Duration.build(*, weeks: number = 0, days: number = 0, hours: number = 0, minutes: number = 0, seconds: number = 0) -> duration`

Adds the named parts. At least one part is required.

### `Duration.parse(text: string) -> duration`

Parses a Go duration such as `"1h30m"`, in whole seconds, or an ISO 8601
duration such as `"PT90S"` or `"P2W"`.

```vibe
Duration.build(hours: 1, minutes: 30).minutes  # 90
Duration.parse("1h30m").to_i                   # 5400
Duration.parse("P2W").days                     # 14
```

## JSON

`JSON` converts between JSON text and values. Parsing keeps member order and
reads integers of any size exactly; stringifying writes members in insertion
order. Both directions cap the text at 1 MiB and nesting at 10,000 arrays and
objects.

### `JSON.parse(text: string) -> any`

Parses JSON into hashes, arrays, strings, ints, floats, bools and `nil`, and
rejects trailing data. The result is `any`: narrow it with `is_type?` or a
cast, or parse with `JSON.parse_as` instead. A duplicate key keeps its last
value.

```vibe
payload = JSON.parse("{\"id\":\"p-1\",\"score\":10}")
if payload.is_type?(:hash)
  record = payload.as(hash<string, any>)
  record["score"]  # 10
end
```

### `JSON.parse_as<T>(text: string, schema: type<T>) -> T`

Parses JSON and checks the result against a type, in one step, as a typed
parameter checks its argument; a mismatch raises the same boundary error. The
type is written as in an annotation: a shape such as `{ name: string, age?: int }`,
`array<int>`, `int?` or a type alias. The result has that type, so later reads
are checked without narrowing. A shape rejects extra fields unless it ends
with `...`.

```vibe
raw = "{\"name\":\"Ada\",\"email\":\"ada@example.com\"}"
user = JSON.parse_as(raw, { name: string, email: string })
user["name"].upcase                      # "ADA"
JSON.parse_as("[1, 2]", array<int>).sum  # 3
```

### `JSON.stringify(value: any) -> string`

Serializes hashes, arrays, strings, numbers, bools and `nil`; symbols and enum
members become strings. Money, durations and times raise, so convert them
first, for example with `to_s` or `iso8601`.

```vibe
JSON.stringify({ id: "p-1", score: 10, tags: ["a", "b"] })
# "{\"id\":\"p-1\",\"score\":10,\"tags\":[\"a\",\"b\"]}"
```

## Math

`Math` holds numeric constants and float functions. Integer arguments are
converted to floats, and every function returns a `float`. Arguments outside
a function's domain raise, such as `Math.sqrt(-1)`; `Math.log(0)` is
`-Infinity`, and a NaN argument gives NaN.

### Constants

- `Math::PI` – the ratio of a circle's circumference to its diameter, a `float`.
- `Math::E` – the base of the natural logarithm, a `float`.

### Functions

- `Math.sqrt(x: number) -> float` – the square root.
- `Math.cbrt(x: number) -> float` – the cube root, also of negative numbers.
- `Math.sin(x: number) -> float` – the sine of an angle in radians.
- `Math.cos(x: number) -> float` – the cosine of an angle in radians.
- `Math.tan(x: number) -> float` – the tangent of an angle in radians.
- `Math.asin(x: number) -> float` – the inverse sine, for `-1 <= x <= 1`.
- `Math.acos(x: number) -> float` – the inverse cosine, for `-1 <= x <= 1`.
- `Math.atan(x: number) -> float` – the inverse tangent.
- `Math.atan2(y: number, x: number) -> float` – the angle of the point
  `(x, y)` from the positive x axis.
- `Math.exp(x: number) -> float` – `Math::E` raised to `x`.
- `Math.log(x: number, base?: number) -> float` – the natural logarithm, or
  the logarithm in `base`.
- `Math.log2(x: number) -> float` – the base-2 logarithm.
- `Math.log10(x: number) -> float` – the base-10 logarithm.
- `Math.hypot(x: number, y: number) -> float` – `sqrt(x ** 2 + y ** 2)`
  without intermediate overflow.

```vibe
Math.sqrt(9)      # 3.0
Math::PI          # 3.141592653589793
Math.hypot(3, 4)  # 5.0
Math.log(8, 2)    # 3.0
```

## Regex

`Regex` builds regex values and runs one-off matches and replacements.
Patterns use RE2 syntax, are at most 16 KiB, and match text of at most 1 MiB.
A regex literal such as `/id-([0-9]+)/i` is a regex value too; its flags are
`i` (ignore case) and `m` (`.` matches line breaks). A regex works with the
`=~` and `!~` operators, `case`/`when` and the string members `match`,
`match?`, `scan`, `sub` and `gsub`.

```vibe
"ID-12" =~ /id-([0-9]+)/i      # 0, the character index of the match
"ID-12" !~ /x/                 # true
"ID-12 ID-34".gsub(/ID-/, "")  # "12 34"
```

### `Regex.new(pattern: string) -> regex`

Compiles `pattern` into a regex value, as a literal without flags would be. An
invalid pattern raises.

### `Regex.escape(text: string) -> string`

`text` with every regex metacharacter escaped, so it matches literally.

### `Regex.union(*patterns: array<string>) -> regex`

A regex matching any of the strings, each literally. Without strings it
matches nothing.

### `Regex.match(pattern: string, text: string) -> string?`

The first match of `pattern` in `text`, or `nil`.

### `Regex.replace(text: string, pattern: string, replacement: string) -> string`

Replaces the first match in `text`; `$1` in `replacement` expands to the first
group.

### `Regex.replace_all(text: string, pattern: string, replacement: string) -> string`

Replaces every match in `text`, expanding `$1` like `Regex.replace`.

```vibe
Regex.match("ID-[0-9]+", "ID-12 ID-34")             # "ID-12"
Regex.replace("ID-12 ID-34", "ID-[0-9]+", "X")      # "X ID-34"
Regex.replace_all("ID-12 ID-34", "ID-[0-9]+", "X")  # "X X"
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1")       # "X-12"
Regex.new("ID-[0-9]+").match?("ID-12")              # true
Regex.escape("a.b*c")                               # "a\\.b\\*c"
Regex.union("cat", "dog").match?("dog")             # true
```

## Regexp

The `Regexp` namespace is removed; `Regex` is the one regex namespace. These
names still run until the static language is the default, but do not compile
with static types.

- `Regexp.new` – removed; use `Regex.new`.
- `Regexp.escape`, `Regexp.quote` – removed; use `Regex.escape`.
- `Regexp.union` – removed; use `Regex.union`.
- `Regexp.last_match` – removed; it was always `nil`. Keep the match data
  that `match` returns instead.

## Hash

The `Hash` namespace is removed; an empty hash is `{}` with a declared type.

- `Hash.new` – removed; write `{}` with a declared type, such as
  `counts: hash<string, int> = {}`.

## Removed spellings

These names are removed; each entry says what to write instead.

### `proc` / `Proc` / `lambda`

Removed: code is not a value. Define a named function and call it, or attach
a block to the call that runs it, as in `people.map { |person| person["name"] }`.
