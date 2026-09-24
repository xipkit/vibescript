# Built-in Functions

Vibescript provides several built-in functions available globally in all scripts.

## Assertions

### `assert(condition, message = nil, message: nil) -> nil`

Raises an error if `condition` is falsy. Use for validating preconditions.

```vibe
def validate_amount(amount)
  assert amount > 0, "amount must be positive"
  amount
end
```

## Output

### `puts(*values) -> nil`

Writes each value to the configured output, one per line, and a single blank
line when called with no arguments. Returns `nil`.

```vibe
puts "processing", 42
```

### `print(*values) -> nil`

Writes each value to the configured output without a trailing newline. Returns
`nil`.

```vibe
print "loading", "..."  # loading...
```

### `p(*values) -> value`

Writes each value in inspect form (strings keep their quotes), one per line,
then returns its argument: the single value for one argument, an array of the
values for several, and `nil` for none. Useful for debug-printing a value
inside a larger expression.

```vibe
count = p(42)  # prints 42, returns 42
p("id", 7)     # prints "id" and 7, returns ["id", 7]
```

### `warn(*values) -> nil`

Writes each value to the configured error output, one per line. Returns `nil`.

```vibe
warn "rate limit nearly reached"
```

## Money

### `money(literal) -> money`

Parses a money value from a string in the format `"amount CURRENCY"`:

```vibe
total = money("100.50 USD")
fee = money("2.50 USD")
net = total - fee  # money("98.00 USD")
```

### `money_cents(cents, currency) -> money`

Creates a money value from an integer cent amount:

```vibe
price = money_cents(2550, "USD")  # $25.50 USD
```

## Time

Time values come from the `now` builtin and the `Time` namespace's
constructors and parser; all of them respect the configured clock and
time zone.

### `now -> string`

Returns the current UTC timestamp as an ISO 8601 / RFC 3339 formatted string:

```vibe
def log_event(name)
  {
    event: name,
    timestamp: now
  }
end

# Returns: { event: "user_signup", timestamp: "2025-01-15T10:30:45Z" }
```

**Note:** The `now` function returns a string, not a time object. This is suitable for logging and timestamping.

For time manipulation in Vibescript, use the `Time` object (`Time.now`, `Time.parse`, `Time.utc`, etc.). See `docs/time.md`.

### Time constructors

The `Time` namespace builds first-class time values. Zone keywords accept IANA
names (`"America/New_York"`), `"UTC"`/`"GMT"`, `"LOCAL"`, or numeric offsets
like `"+05:30"`. See [Time](time.md) for the full instance-method surface.

- `Time.now(in: zone)` – the current time as a time value, optionally in a
  zone.
- `Time.new(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, zone = nil, in: zone)`
  – builds a calendar time; omitted fields default to January 1 at midnight.
- `Time.local(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, usec = 0)` /
  `Time.mktime(...)` – calendar time in the host's local zone.
- `Time.utc(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, usec = 0)` /
  `Time.gm(...)` – calendar time in UTC.
- `Time.at(seconds, subsec = nil, unit = nil, in: zone)` – time from epoch
  seconds, with Ruby-style subsecond arguments.
- `Time.parse(string, layout = nil, in: zone)` – parses common formats
  (RFC3339/RFC1123, `YYYY-MM-DD`, ...) or an explicit Go layout.

```vibe
Time.utc(2024).iso8601  # "2024-01-01T00:00:00Z"
```

## Control Flow

### `loop { ... } -> value`

Runs the block until it exits with `break`. A `break value` becomes the return
value, and `next` skips to the next iteration.

```vibe
x = 0
value = loop do
  x = x + 1
  if x == 3
    break :done
  end
end
```

## Removed Callable Constructors

### `proc { |args| ... }` / `Proc` / `lambda { |args| ... }`

Removed, including `Proc.new`. Executable code is not a value: each of these
names now fails with an error naming the replacement. Define a named function and call it, or attach a
block to the call that runs it, as in `people.map { |person| person.name }`.
See [Blocks](blocks.md) for the supported block forms.

## Formatting

### `format(pattern, *values) -> string` / `sprintf(pattern, *values) -> string`

Formats values with Ruby-style percent format strings for common numeric and
string cases. `String#%` uses the same formatter. Output is capped at 1 MiB
before width or precision padding is materialized.

```vibe
"%s:%03d" % ["id", 7]  # "id:007"
format("%.2f", 1.234)  # "1.23"
sprintf("%x", 255)     # "ff"
```

## Random IDs

### `uuid -> string`

Returns an RFC 9562 version 7 UUID string:

```vibe
event_id = uuid
```

### `random_id(length = 16) -> string`

Returns an alphanumeric random identifier string:

```vibe
short = random_id(8)
token = random_id()
```

### `rand(max = nil) -> number`

Returns a random float in `[0.0, 1.0)` with no argument, an integer in
`0...max` for a positive integer bound, or an integer inside an integer range.

```vibe
rand < 1.0
rand(10)
rand(1..3)
```

### `srand(seed = nil) -> int | nil`

Seeds the current script call's `rand` sequence. Reusing the same integer seed
inside a call gives the same sequence without leaking seeded state into later
calls.

```vibe
srand(1234)
[rand, rand(10), rand(1..3)]
```

## Numeric Conversion

### `to_int(value) -> int`

Converts `int`, integral `float`, or base-10 numeric `string` values into `int`.

### `to_float(value) -> float`

Converts `int`, `float`, or numeric `string` values into `float`.

```vibe
count = to_int("42")
ratio = to_float("1.25")
```

## Math

The `Math` namespace mirrors Ruby's `Math` module: transcendental constants and
pure numeric helpers backed by the host's math library. Constants read with
either accessor (`Math::PI` or `Math.PI`) and helpers are called like
`Math.sqrt(9)`. Integer arguments are promoted to floats and every helper
returns a `float`, just like Ruby where `Math` always yields a `Float`.

### Constants

- `Math::PI` – the ratio of a circle's circumference to its diameter.
- `Math::E` – the base of the natural logarithm.

### Functions

- `Math.sqrt(x)` / `Math.cbrt(x)` – square and cube roots.
- `Math.sin(x)`, `Math.cos(x)`, `Math.tan(x)` – trigonometric functions
  (radians).
- `Math.asin(x)`, `Math.acos(x)`, `Math.atan(x)` – inverse trigonometric
  functions; `asin`/`acos` require `-1 <= x <= 1`.
- `Math.atan2(y, x)` – angle of the point `(x, y)` from the positive x-axis.
- `Math.exp(x)` – `E` raised to `x`.
- `Math.log(x)` / `Math.log(x, base)` – natural logarithm, or the logarithm in
  the given base.
- `Math.log2(x)` / `Math.log10(x)` – base-2 and base-10 logarithms.
- `Math.hypot(x, y)` – `sqrt(x**2 + y**2)` without intermediate overflow.

```vibe
Math.sqrt(9)        # 3.0
Math::PI            # 3.141592653589793
Math.hypot(3, 4)    # 5.0
Math.log(8, 2)      # 3.0
```

Arguments outside a function's mathematical domain raise a domain error (for
example `Math.sqrt(-1)`, `Math.asin(2)`, or `Math.asin(Float::INFINITY)`),
matching Ruby's `Math::DomainError`. In-domain special values follow Ruby and
IEEE 754: `Math.log(0)` returns `-Infinity`, `Math.sin`/`cos`/`tan` of
`Infinity` return `NaN`, and a `NaN` argument propagates through unchanged.

## Duration

Duration values usually come from duration literals (`5.minutes`, `2.days`);
the `Duration` namespace builds them from numbers and strings. See
[Durations](durations.md) for the full instance-method surface.

### `Duration.build(seconds)` / `Duration.build(weeks:, days:, hours:, minutes:, seconds:)`

Builds a duration from total seconds or from named parts. At least one part is
required (a bare `Duration.build()` errors), and positional seconds and named
parts are mutually exclusive.

### `Duration.parse(string)`

Parses Go duration strings (`"1h30m"`, whole seconds only) or ISO 8601
durations (`"PT90S"`, `"P2W"`).

```vibe
Duration.build(hours: 1, minutes: 30).minutes # 90
Duration.parse("1h30m").seconds               # 5400
```

## JSON

`JSON` converts between JSON text and Vibescript values: parsing preserves
member order and reads oversized integers exactly, and stringifying emits
members in insertion order.

### `JSON.parse(string)`

Parses a JSON string into Vibescript values (`hash`, `array`, `string`, `int`,
`float`, `bool`, `nil`):

```vibe
payload = JSON.parse("{\"id\":\"p-1\",\"score\":10}")
payload["score"] # 10
```

**A round trip preserves lookup.** Hash keys live in one string keyspace, so a
parsed object is equal to the literal it came from and both key spellings read
the same entry:

```vibe
obj  = { name: "Ada" }
back = JSON.parse(JSON.stringify(obj))
back["name"]  # "Ada"
back[:name]   # "Ada"
back == obj   # true
```

`JSON.parse` enforces a 1 MiB input limit and rejects more than 10,000 nested
arrays/objects.

### `JSON.parse_as(string, shape)`

Parses a JSON string and validates the result against a shape in one step,
with the same semantics as typed parameter boundaries. The static checker
treats the result as that shape, so downstream reads are inferred and checked
without further annotations. Validation failures raise the standard
typed-boundary error. See [Typing](typing.md#jsonparse_as).

```vibe
body = JSON.parse_as(raw, { name: string, email: string })
body["name"]  # a known string
```

### `JSON.stringify(value)`

Serializes supported values (`hash`/`object`, `array`, scalar primitives) into
a JSON string:

```vibe
raw = JSON.stringify({ id: "p-1", score: 10, tags: ["a", "b"] })
```

`JSON.stringify` enforces a 1 MiB output limit and rejects more than 10,000
nested arrays/objects.

## Regex

Regex patterns are quoted strings or Ruby-style `/pattern/flags` regex
literals. A literal produces a first-class regex value with `source`, `flags`,
`match`, and `match?` members, works with the `=~` and `!~` match operators and
`case`/`when` matching, and is accepted by the string pattern helpers
(`match`, `match?`, `scan`, `sub`, `gsub`). Supported flags are `i`
(case-insensitive) and `m` (`.` matches newlines); patterns use Go's RE2
syntax, exactly like quoted string patterns.

```vibe
"ID-12" =~ /id-([0-9]+)/i     # 0 (character index of the match, nil when none)
"ID-12" !~ /x/                # true
/id-([0-9]+)/i.match("ID-12") # match data: m[0] "ID-12", m[1] "12"
"ID-12 ID-34".gsub(/ID-/, "") # "12 34"
"ID-12".match /id-([0-9]+)/i  # parenless command argument, same as match(...)
```

A regex literal also works as a parenless command argument, matching Ruby:
after a callee that is not a local variable, a space before the slash with
none after it opens the literal, so `text.scan /a+/` is `text.scan(/a+/)`. A
slash after a local variable always divides (`total /2`), as does a slash
spaced on both sides or flush (`f / 2`, `f/2`). See the
[language reference](language_reference.md#method-calls) for the full spacing
rule.

### `Regex.match(pattern, text)`

Returns the first match string or `nil` when no match exists.

### `Regex.replace(text, pattern, replacement)`

Replaces the first regex match in `text`.

### `Regex.replace_all(text, pattern, replacement)`

Replaces all regex matches in `text`.

```vibe
Regex.match("ID-[0-9]+", "ID-12 ID-34")                  # "ID-12"
Regex.replace("ID-12 ID-34", "ID-[0-9]+", "X")           # "X ID-34"
Regex.replace_all("ID-12 ID-34", "ID-[0-9]+", "X")       # "X X"
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1")            # "X-12"
```

Regex helpers enforce input guards (max pattern size 16 KiB, max text size 1 MiB).

### `Regexp.new(pattern)`

Compiles a pattern string into a first-class regex value, equivalent to a
`/pattern/` literal without flags.

### `Regexp.escape(text)` / `Regexp.quote(text)`

Returns `text` with every regex metacharacter escaped, so the result matches
the text literally when used as a pattern.

### `Regexp.union(*patterns)`

Builds a regex value that matches any of the given pattern strings.

### `Regexp.last_match`

Returns `nil`: Vibescript does not track Ruby's global per-call match state.
The member exists for Ruby compatibility; use `regex.match(text)` to obtain
match data directly.

```vibe
Regexp.new("ID-[0-9]+").match?("ID-12")   # true
Regexp.escape("a.b*c")                    # "a\\.b\\*c"
Regexp.union("cat", "dog").match?("dog")  # true
```

## Hash

`Hash` constructs an empty hash.

### `Hash.new`

Builds an empty hash, identical to a `{}` literal. Hashes carry no per-hash
default, so `Hash.new` takes no argument and no block; a missing key reads as
`nil` and `fetch` supplies a fallback per lookup. See
[Missing keys](hashes.md#missing-keys).

```vibe
Hash.new                     # {}
Hash.new[:missing]           # nil
Hash.new.fetch(:missing, 0)  # 0
```

## Module Loading

### `require(module_name, as: nil) -> object`

Loads a module from configured module search paths and returns a namespace
object containing its exported function names and enums. Exported functions are
called directly through that namespace; they are not detachable function
values. Module functions are exported by default, and top-level enums are
exported as well. Executable top-level statements run as the module initializer
before exports are returned, so module-local values can be prepared for later
calls. `private def ...` keeps helper functions module-local. Exported names are
injected into globals only when the name is still free (existing globals keep
precedence), and `as:` can bind the namespace explicitly:

```vibe
def calculate_total(amount)
  require("fee_calculator", as: "helpers")
  amount + helpers.calculate_fee(amount)
end
```

See `examples/module_require.md` for detailed usage patterns.
