# Vibescript language guide

Vibescript is a small, statically typed scripting language for workflows that a host program embeds and runs under step, memory and time limits. This guide is written for authors, human or AI, who need the whole language on one page. Put it in a model's context next to the output of `vibes prelude`, which lists every builtin function and member with its exact signature; hosts extend that listing with their own functions, capabilities and globals through `Engine::prelude`.

Every example here compiles with static types. The design is recorded in [ADR-007](adr/007-static-types.md) (static types) and [ADR-008](adr/008-canonical-surface-for-ai-authors.md) (one spelling for everything).

## The rules in brief

- Every function parameter has a type, and every function that returns a value declares `-> T`.
- A local takes the type of its first assignment and keeps it. Empty literals and `nil` need a declared type: `names: array<string> = []`.
- Conditions are `bool`. Test optional values with `x == nil` or `x != nil`; there is no truthiness.
- `items[i]` and `hash[key]` are optional (`T?`). Use `fetch` when the value must be there.
- Records are shapes, `{ name: string, age: int }`, read with literal keys: `user["name"]`. Dictionaries are `hash<string, V>`. Hash keys are strings; a dot only calls methods.
- Values of type `any`, such as `JSON.parse` results, must be narrowed with `is_type?`, `.as(T)` or `JSON.parse_as` before use.
- Blocks use braces only. A function that yields declares its block: `&block: int -> bool`.
- Keyword parameters follow a bare `*`: `def send(to: string, *, retries: int = 3)`.
- `//` is integer floor division; `/` is true division.
- Each operation has exactly one spelling: `length`, not `size`; `is_type?`, not `is_a?`; `if !x`, not `unless x`. `vibes fix` rewrites removed spellings.

## Programs

A script is a sequence of top-level statements and declarations: functions, classes, enums, modules, constants and type aliases. The host either runs the top-level statements or calls a function by name, passing arguments that are checked against its parameter types.

```vibe
LIMIT = 3

def greet(name: string, times: int = 1) -> string
  ("Hello, #{name}! " * times).strip
end

puts greet("Ada")            # Hello, Ada!
puts greet("Ada", LIMIT)
```

Declarations may appear in any order; each function is checked once, from its own signature and the signatures of what it calls. Comments start with `#`. Statements end at a line break; a line ending in an operator, a comma or an opening bracket continues on the next line.

`vibes run script.vibe` runs a file, `vibes run --function greet script.vibe Ada` calls one function with command-line arguments, which are strings, and `vibes check` compiles and reports diagnostics without running anything. See [the command line](cli.md).

## Values and literals

```vibe
count = 1_000                 # int, arbitrary precision: 2 ** 100 stays exact
ratio = 0.25                  # float
mask = 0xff                   # also 0b1010 and 0o17
name = "Ada"                  # string; double quotes interpolate
raw = 'no #{interpolation}'   # single quotes do not
greeting = "Hi #{name}"       # interpolation calls to_s
state = :ready                # symbol
done = false                  # bool
nothing: string? = nil        # nil needs a declared type
tags = ["a", "b"]             # array<string>
user = { name: "Ada", age: 36 } # shape { name: string, age: int }
span = 1..10                  # range; 1...10 excludes 10
timeout = 5.minutes           # duration; also seconds, hours, days, weeks
price = money("12.50 USD")    # money
pattern = /id-([0-9]+)/i      # regex
```

Strings are immutable byte strings, usually UTF-8, and character positions count Unicode code points. Integers never overflow; they grow as needed. Floats that are whole numbers print without a fractional part, so `p 3.0` prints `3`.

## Types

| Type | Meaning |
| --- | --- |
| `int`, `float`, `string`, `symbol`, `bool`, `nil` | Scalars. `number` is `int \| float`. |
| `duration`, `time`, `money`, `range`, `regex` | Built-in value types. |
| `T?` | `T` or `nil`. |
| `A \| B` | Either type. |
| `array<T>` | An array whose elements are `T`. |
| `hash<string, V>` | A dictionary from string keys to `V`. |
| `{ name: string, age?: int }` | A shape: a record with these keys. `age?:` may be absent. |
| `[A, B]` | A tuple: an array of exactly an `A` then a `B`. |
| `Status`, `Account` | Enums and classes you declare. |
| `any` | A value not known statically. It must be narrowed before use. |

`type Point = { x: int, y: int }` names a type. Aliases are transparent: `Point` and the shape are the same type. See [types](types.md) for the complete syntax.

## Locals

A local is declared by its first assignment and keeps that type everywhere, including inside blocks and branches. Declare the type explicitly when the first value does not determine it: `nil`, `[]`, `{}`, or a value that should be stored as a wider union.

```vibe
total = 0                          # int from here on
total += 5
names: array<string> = []          # an empty literal needs a type
names << "Ada"
counts: hash<string, int> = {}
counts["draft"] = 1
label: string? = nil               # so does nil
label = "ready"
amount: int | float = 1            # a union must be declared
amount = 2.5
```

Assignment never converts: an `int` local cannot hold a `float`. A local must be assigned on every path before it is read (V0202):

```vibe error=V0202
if Time.now.hour < 12
  greeting = "morning"
end
puts greeting
```

Assign it before the branch, or in every branch.

## Functions

```vibe
def fee(amount: int, *, rate: int = 5, minimum: int = 1) -> int
  charge = amount * rate // 100
  charge < minimum ? minimum : charge
end

def log(message: string)
  puts "[log] #{message}"
end

fee(250)                      # 12
fee(250, rate: 10)            # 25
fee 40, minimum: 3            # 3: parentheses are optional with arguments
log "sent"
```

- Every parameter declares its type. Optional parameters have defaults, `times: int = 1`.
- Parameters after a bare `*`, or after a rest parameter `*items: array<int>`, are keywords, passed by name. `**options: hash<string, int>` collects extra keywords.
- `-> T` declares the result, which is the last expression or an explicit `return`. A function without `->` returns `nil`, and `return value` in it is an error (V0117).
- A call without arguments has no parentheses: `items.length`, `uuid`, `Time.now`, `log_all`.
- `return a, b` returns a tuple when the result type is one, such as `-> [int, int]`; `q, r = divide(7, 2)` destructures it.
- Functions are not values. They cannot be stored, passed or returned; pass a block instead, or choose between calls with `if` or `case`. Functions are not generic either: a helper that must accept several types takes `any` or a union and narrows.

## Blocks

A block is code attached to a call, in braces, on one line or several. It runs while the call runs and never escapes it.

```vibe
prices = [3, 8, 12]
doubled = prices.map { |p| p * 2 }             # [6, 16, 24]
cheap = prices.select { |p| p < 10 }           # [3, 8]
total = 0
prices.each { |p|
  next if p > 10
  total += p                                   # blocks can update outer locals
}
first_big = prices.find { |p| p > 5 }          # 8, an int?
```

Block parameters take their types from the called function's signature; annotations such as `|p: int|` are optional and must match. `next value` ends one call of the block, and `break value` ends the whole call with that value.

A function that yields declares its block as its last parameter. The block's name is a declaration only; `yield` and `block_given?` are the only ways to use it.

```vibe
def keep(items: array<int>, &block: int -> bool) -> array<int>
  kept: array<int> = []
  items.each { |item|
    kept << item if yield(item)
  }
  kept
end

def each_pair(counts: hash<string, int>, &block: (string, int))
  counts.keys.each { |key| yield key, counts.fetch(key) }
end

def maybe_log(message: string, &block?: string -> string)
  if block_given?
    puts yield(message)
  else
    puts message
  end
end

keep([1, 2, 3, 4]) { |n| n.even? }             # [2, 4]
each_pair({ a: 1 }) { |key, value| puts "#{key}=#{value}" }
maybe_log("ready") { |text| text.upcase }
```

`&block: int` takes an `int` and returns nothing; `&block: (string, int)` takes two values; `-> R` makes `yield` an expression of type `R`. With `&block?:` the block is optional and every `yield` must be guarded by `block_given?` (V0307).

## Control flow

```vibe
score = 72
grade = if score >= 90
          "A"
        elsif score >= 70
          "B"
        else
          "C"
        end

ok = score > 50 && !(score == 60)
note = ok ? "pass" : "fail"
puts note if ok                       # statement modifiers: if, while

i = 0
while i < 3
  i += 1
end

for n in 1..3
  puts n
end

result = loop {
  i += 1
  break i if i > 5
}
```

Conditions of `if`, `elsif`, `while`, the ternary and statement modifiers are `bool`, and so are the operands of `!`, `&&` and `||` (V0104, V0105). `if`, `case`, `while`, `for` and `begin` are expressions. There is no `unless` or `until`: write `if !cond` and `while !cond`.

`case` compares its subject with each `when` value in order, using `==` for values, membership for ranges and matching for regexes. Each `when` has one expression, not a statement list; call a function or use `if` when a branch needs several statements. Without a subject, `case` takes the first `when` whose condition is true.

```vibe
def size_label(n: int) -> string
  case n
  when 0 then "none"
  when 1..9 then "few"
  else "many"
  end
end

def sign(n: int) -> string
  case
  when n < 0 then "negative"
  when n == 0 then "zero"
  else "positive"
  end
end
```

## Nil and optional values

`T?` values come from declarations and from operations that can miss: `array[i]`, `hash[key]`, `find`, `first`, `index`, `match`, `x&.m`. Using one where `nil` is not accepted is an error (V0107). Handle it with a nil test or an early return, which narrow the local, with `fetch`, which raises instead, or with `&.`, which calls through `nil`:

```vibe
def initial(name: string?) -> string
  return "?" if name == nil            # an early return narrows name to string
  letter = name[0]                     # string?, since the name may be empty
  letter == nil ? "?" : letter.upcase
end

scores = [90, 72]
best = scores.max
if best != nil                         # a nil test narrows best in the branch
  puts best + 1
end

first = scores.fetch(0)                # fetch raises instead of returning nil
label = scores.first&.to_s             # &. calls through nil; label is string?
fallback = label == nil ? "none" : label
```

Narrowing applies to locals and parameters. A member read or index expression is not narrowed, because it could change between the test and the use; bind it to a local first:

```vibe
user = JSON.parse_as("{\"nick\": null}", { nick: string? })
nick = user["nick"]
if nick != nil
  puts nick.upcase
end
```

## Arrays

```vibe
items = [3, 1, 2]                   # array<int>
mixed = [1, "a"]                    # array<int | string>
items << 4                          # push one element
items.push(5, 6)
sorted = items.sort                 # [1, 2, 3, 4, 5, 6]; items is unchanged
third = items[2]                    # int?
third_or_zero = items.fetch(2, 0)   # int
last_two = items.last(2)            # array<int>
evens, odds = items.partition { |n| n.even? }
pairs: array<[string, int]> = [["a", 1], ["b", 2]]
lookup = pairs.to_h                 # hash<string, int>
```

Arrays and hashes are values: assigning one to another local, passing it or storing it makes an independent copy, so an update through one name is never visible through another. Updating members such as `push`, `pop` and `<<`, and index assignment such as `items[0] = x`, change the local, field or nested path they name. A write through two indexes, such as `grid[0][1] = 9`, does not compile, because `grid[0]` is optional: read the row with `fetch`, update it, and store it back with `grid[0] = row`. `sort` on `array<int | string>` is an error (V0115): members with bounds, such as `sort`, `sum` and `max`, need one element type. `fill` and `insert` raise past the end of an array.

## Shapes and dictionaries

A hash literal is a shape: a record whose keys are fixed. Its fields are read and written with literal string keys, and reading a declared field gives the field's type, not an optional:

```vibe
order = { id: "A-1", total: 30, note: "gift" }
order["total"] += 5
summary = "#{order["id"]}: #{order["total"]}"
```

Reading a key a shape does not declare is an error (V0110), and so is indexing it with a key known only at runtime (V0111): a record is not a dictionary. A dictionary is declared as `hash<string, V>`, and its reads are optional:

```vibe
stock: hash<string, int> = { apple: 3, pear: 0 }
name = "kiwi"
count = stock[name]                  # int?
stock[name] = 7
in_stock = stock.select { |fruit, n| n > 0 }.keys
total = stock.values.sum
```

A shape whose fields all have type `V` can be passed where `hash<string, V>` is expected. Keys are always strings: `{ apple: 3 }` has the key `"apple"`, symbols are not keys, and `h.name` never reads a field (V0415).

Tuples give fixed-length arrays their element types: `divmod` returns `[int, int]`, `partition` returns `[array<T>, array<T>]`, and a hash's `to_a` returns `array<[string, V]>`. Indexing a tuple with an integer literal gives that element's type.

## Dynamic values

`any` is the type of values the program cannot know statically: `JSON.parse` results, globals a host declares without a type, and results of host functions without signatures. An `any` value may be compared with `==`, tested with `== nil` or `is_type?`, interpolated, and passed where `any` is accepted. Anything else, such as calling a member, indexing or arithmetic, is an error until it is narrowed (V0106).

```vibe
raw = "{\"name\": \"Ada\", \"tags\": [\"admin\"], \"age\": 36}"

user = JSON.parse_as(raw, { name: string, tags: array<string>, age?: int })
user["name"].upcase                 # "ADA", checked when parsed

data = JSON.parse(raw)              # any
if data.is_type?(:hash)
  record = data.as(hash<string, any>)
  age = record.fetch("age").as(int) # a checked cast raises on a mismatch
  puts age + 1
end

def describe(value: int | string) -> string
  if value.is_type?(:int)
    "number #{value + 1}"
  else
    "text #{value.upcase}"
  end
end
```

`is_type?(:atom)` in a condition narrows a local or parameter of type `any` or a union. `value.as(T)` and `JSON.parse_as(text, T)` validate at runtime against any type an annotation can name, raise the typed boundary error on a mismatch, and have type `T`. Prefer `JSON.parse_as` for input with a known structure.

## Classes and enums

```vibe
enum Status
  Draft
  Published
end

class Post
  getter title: string
  property status: Status
  @views: int = 0
  @@count: int = 0

  def initialize(@title: string, status: Status = :draft)
    @status = status
    @@count += 1
  end

  def self.count -> int
    @@count
  end

  def view -> int
    @views += 1
    @views
  end

  def publish
    @status = Status::Published
  end

  def published? -> bool
    @status == Status::Published
  end
end

post = Post.new("Hello")
post.view
post.publish
post.status = :draft
label = case post.status
        when Status::Draft then "draft"
        when Status::Published then "live"
        end
```

- Every instance variable is declared: in the class body (`@views: int = 0`, or `@name: string` assigned by `initialize`), or by `getter`, `setter` or `property`. Reading an undeclared one is an error (V0204), and one without a default must be assigned on every path through `initialize` (V0205). `@title: string` in a parameter list assigns that field.
- Class variables are declared with a value, `@@count: int = 0`. Class methods are `def self.name`. Uppercase assignments in the body, such as `LIMIT = 3`, are constants, read as `Post::LIMIT` outside.
- Classes have no inheritance, and instances have identity: two names for the same instance see the same changes. Classes can define operators, `==`, `to_s`, `[]` and `[]=`. See [classes](classes.md).
- An enum is a type. A symbol literal naming a member, such as `:draft`, is accepted wherever that enum is expected; otherwise write `Status::Draft`. Members have `name`, `symbol` and `to_s`.
- `case` over an enum or a `bool` must handle every member or have an `else` (V0114), so adding a member shows every `case` that needs it. Its `when` values name members as `Status::Draft`.

## Modules and required files

A module is a namespace of functions, constants and nested modules or classes:

```vibe
module Pricing
  TAX_PERCENT = 8

  def self.with_tax(cents: int) -> int
    cents + cents * TAX_PERCENT // 100
  end
end

Pricing.with_tax(1_000)   # 1080
```

`require` loads another file from the host's module paths. The name and the optional alias are string literals (V0309), so the compiler checks the file, and every call into it, before the script runs. A file's `def` and `export def` functions are public, `private def` functions are not, and public functions are also bound by name in the requiring script when the name is free:

```vibe module=reports/format.vibe
export def cents(amount: int) -> string
  format("%d.%02d", amount // 100, amount % 100)
end
```

```vibe
fmt = require("reports/format")
fmt.cents(1234)                          # "12.34"
require("reports/format", as: "report")
report.cents(5)                          # "0.05"
cents(99)                                # "0.99"
```

## Host values

A script reaches the host only through what the host declares. Arguments to the function a host calls are checked against its parameter types when the call starts. Globals and capabilities are declared by the host (`Engine::declare_global`, `Engine::declare_capability`) and appear in `Engine::prelude` with their types; a bare name that is neither in scope nor declared is a compile error (V0201). A global declared as `customer: { name: string, tier: string }` is used like any other typed value:

```vibe global=customer:{name:string,tier:string}
greeting = "Hello, #{customer["name"]}"
discount = customer["tier"] == "gold" ? 10 : 0
```

A capability's methods are typed by the signatures the host publishes; a host function or capability method without a signature takes and returns `any`. See [host globals](globals.md) and [host capabilities](capabilities.md).

## Errors

```vibe
def parse_amount(text: string) -> int
  raise ArgumentError, "empty amount" if text.empty?
  text.to_i
end

def safe_amount(text: string) -> int
  begin
    parse_amount(text)
  rescue ArgumentError => error
    warn error.message
    0
  ensure
    puts "parsed #{text}"
  end
end

value = parse_amount("x") rescue -1      # a rescue modifier gives a fallback
assert value >= -1, "unexpected value"
```

`raise "message"` raises a `RuntimeError`; `raise Class, "message"` names the class. `rescue` clauses match in order and may list several classes, `rescue TypeError | ArgumentError => error`; a bare `rescue` catches ordinary errors. The rescued value has type `error`, with `message`, `class`, `backtrace` and `code_frame`. `else` runs after the body succeeds, `ensure` always runs, and `retry` restarts the body. `assert(condition, message)` raises an `AssertionError` when the condition is false. Running out of steps, memory or time cannot be rescued. See [error handling](errors.md).

## Operators

| Operators | Meaning |
| --- | --- |
| `+ - * **` | Arithmetic on `int` and `float`; `int` never overflows. `+` also joins two strings or two arrays; `*` repeats a string. |
| `/` | True division: `7.0 / 2` is `3.5`. Until the switchover to static types by default, `/` on two ints is rejected (V0109) so that no existing floor division changes meaning silently. |
| `//` | Floor division: `7 // 2` is `3`, `-7 // 2` is `-4`. |
| `%` | Remainder with the sign of the divisor, consistent with `//`: `-7 % 3` is `2`. On a string, `"%05.1f" % [3.14159]` formats. |
| `== !=` | Equality by value; `1 == 1.0` is true, and large integers compare exactly. |
| `< <= > >= <=>` | Ordering of numbers, strings, symbols, times, durations and money. |
| `&& \|\| !` | Boolean logic on `bool` only, short-circuiting. |
| `=~ !~` | Regex match: the character index of the match, or `nil`, and its negation. |
| `<<` | Appends one element to an array. |
| `.. ...` | Inclusive and exclusive ranges. |
| `&.` | Calls through an optional value, giving `nil` when it is `nil`. |
| `+= -= *= %= **=` | Compound assignment; `&&=` and `\|\|=` take `bool` targets. |

Money adds and subtracts money of the same currency and multiplies by integers. Durations add to times and to each other: `Time.now + 2.hours`, `5.minutes.ago`, `3.days.after(start)`.

Integer division by zero raises `ZeroDivisionError`.

## What the checker guarantees

A program that compiles has no type errors except where dynamic data enters it. The checker proves that:

- every call names a function or member that exists, with arguments, keywords and block matching one of its signatures;
- every value has the type its position expects, including function results, fields, collection elements and block results;
- optional values are tested before they are used, and `any` values are narrowed;
- locals are assigned before they are read and keep one type;
- shapes are read only at keys they declare;
- `case` over an enum or `bool` is exhaustive;
- a private method is called only without a receiver, and a protected one only from its own class's methods (V0208);
- every `require` resolves, and calls into required files match their declarations.

What remains at runtime: `fetch`, `as` and `JSON.parse_as` raise when the value is not there or not of the type; arithmetic raises on division by zero and invalid operands such as an out-of-range float conversion; builtins raise on invalid arguments such as a negative count; values a host passes in are checked against the declared parameter, global and capability types when a call starts; and every call runs under its step, memory, recursion and time limits.

## Diagnostics and fixes

Every compile error has a stable code, a span and, where they apply, the expected and found types. Codes are grouped by area: `V0001` syntax, `V01xx` types, `V02xx` names, `V03xx` calls and `V04xx` removed spellings. See [diagnostics](diagnostics.md).

```sh
vibes check --static script.vibe   # human-readable diagnostics
vibes check --json script.vibe     # one JSON object per diagnostic
vibes fix script.vibe              # apply every machine-applicable fix
vibes prelude                      # every builtin signature
```

`vibes check --json` gives each diagnostic's code, message, span and fixes; a fix is a set of text edits, offered only when the repair is unambiguous. A model repairing a script can apply the fixes and read the remaining messages, which name the expected and found types and usually the construct to write instead.

## Removed spellings

These are compile errors with static types, and `vibes fix` rewrites most of them. `src/signatures/renames.txt` is the complete table.

| Write | Not |
| --- | --- |
| `items.length` | `size`, `count` without arguments |
| `h.key?("k")`, `h.value?(v)` | `has_key?`, `member?`, `include?` on a hash, `has_value?` |
| `h["name"]` | `h[:name]`, `h.name` |
| `x == nil`, `x != nil` | `x.nil?`, `if x` |
| `x.is_type?(:int)` | `is_a?`, `kind_of?`, `instance_of?` |
| `x.to_s`, `x.to_sym`, `x.dup` | `string`, `id2name`, `intern`, `clone` |
| `a == b` | `eql?`, `equal?` |
| `if !cond`, `while !cond` | `unless`, `until` |
| `{ \|x\| ... }` | `do \|x\| ... end` |
| `["a", "b"]` | `%w[a b]` |
| `counts: hash<string, int> = {}` | `Hash.new` |
| `format(...)`, `Regex.new(...)`, `Time.now.iso8601` | `sprintf`, `Regexp.new`, `now` |
| `5.minutes`, `5.minutes.ago`, `5.minutes.before(t)` | `5.minute`, `5.minutes.ago(t)` |
| `items.index(x)`, `items.first(2)`, `items.push(x)`, `items.prepend(x)` | `find_index`, `take`, `append`, `unshift` |
| `uuid`, `x.length` | `uuid()`, `x.length()` |
| `JSON.parse(x)`, `Pricing.with_tax(1)` | `JSON::parse(x)`, `Pricing::with_tax(1)` |
| `def f(x: int, *, retries: int = 3)` | `def f(x: int, retries: 3)` |
| `7 // 2` for floor division | `7 / 2` on ints |
| a direct call, or `case` over an enum | `send`, `public_send`, `respond_to?` |
