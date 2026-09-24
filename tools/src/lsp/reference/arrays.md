# Arrays in Vibescript

Arrays are ordered collections. Use square brackets to declare literals:

```vibe
players = ["alex", "maya", "li"]
```

For static string or symbol lists, percent array literals are also supported:

```vibe
words = %w[alpha beta gamma]
statuses = %i[draft published archived]
```

The lowercase forms (`%w`, `%i`) are literal: whitespace separates the entries
and only delimiters, whitespace, and the backslash itself can be escaped, so a
sequence such as `\n` stays as the two characters `\` and `n`.

The uppercase forms (`%W`, `%I`) apply double-quoted string semantics to each
entry. They expand `#{...}` interpolation and process the usual escape
sequences (`\t`, `\n`, and so on), while still splitting entries on whitespace
that is neither escaped nor inside an interpolation. `%W` produces strings and
`%I` produces symbols:

```vibe
name = "Ada"
greeting = %W[hello #{name} world]      # ["hello", "Ada", "world"]
labels   = %I[hello #{name} world]      # [:hello, :Ada, :world]
spaced   = %W[a #{1 + 2} d]             # ["a", "3", "d"]
escaped  = %W[tab\there a\ b]           # ["tab\there", "a b"]
```

Any of `[]`, `()`, `{}`, `<>`, or a repeated non-alphanumeric delimiter work
for every form (`%W(a b)`, `%I{x y}`).

## Transformations

Common enumerable helpers include:

- `map` to transform elements.
- `map_with_index` to transform elements while also passing each element's
  0-based index to the block (`["a", "b"].map_with_index { |value, index| [value, index] }`
  is `[["a", 0], ["b", 1]]`). It takes no arguments and requires a block.
- `filter_map` to transform elements and keep only the truthy results in one
  pass, dropping falsy block returns (the fused equivalent of `map` then a
  truthiness filter).
- `flat_map` (alias `collect_concat`) to transform elements and concatenate the
  results, flattening exactly one level: an array result contributes its
  elements and anything else contributes itself
  (`[[1, 2], [3]].flat_map { |row| row }` is `[1, 2, 3]`). It takes no arguments
  and requires a block. A nested array inside a result is left alone, so it is
  `map` then `flatten(1)` rather than a full flatten.
- `select` to keep items the block accepts.
- `reject` to keep items the block rejects (the inverse of `select`).
- `find` to locate the first matching item.
- `find_index(value)` / `find_index { ... }` to locate the first matching index.
- `reduce` to accumulate values, either with a block or with a symbol/string
  operation shorthand (`[1, 2, 3].reduce(:+)`, `["a", "b"].reduce(:concat)`).
- `first` / `last` to read an end element, or `first(n)` / `last(n)` to slice without mutating. The optional count is the only argument they accept; passing more than one positional argument or any keyword argument raises.
- `take(n)` / `drop(n)` to keep or skip a prefix; both reject negative counts.
- `zip(*arrays)` to combine arrays element-wise into rows, padding short arrays with `nil`.
- `transpose` to swap the rows and columns of a matrix of equal-length array rows; it raises when a row is not an array or the rows differ in length.
- `push(*values)` appends the values to the receiver in place and returns the receiver, matching Ruby. `append(*values)` is the Ruby-style alias. Bare `push` and `push()` are no-ops that return the receiver.
- `array << value` is the Ruby-style shovel operator: it appends the single value to the receiver in place and returns the receiver (`[1, 2] << 3` is `[1, 2, 3]`), so a bare `values << value` statement accumulates and chains like Ruby's. The left operand must be an array.
- `pop` / `pop(n)` removes element(s) from the end of the receiver in place. Bare `pop` returns the removed element (`nil` on an empty array); `pop(n)` removes up to `n` elements and returns them as an array in receiver order.
- `prepend(*values)` inserts the values at the front of the receiver in place and returns the receiver (`[3].prepend(1, 2)` is `[1, 2, 3]`). `unshift(*values)` is a Ruby-style alias.
- `shift` / `shift(n)` removes element(s) from the front of the receiver in place, mirroring `pop`: bare `shift` returns the removed element (`nil` on an empty array) and `shift(n)` removes up to `n` and returns them as an array. `n` must be a non-negative integer.
- `delete(value)` removes every element equal to `value` from the receiver in place. Following Ruby, it returns the last removed element when at least one match was removed and `nil` otherwise; when an element is equal to but a distinct object from `value` you get back the stored element, not your search argument. `delete(value) { default }` returns the block result on a miss instead, leaving the receiver untouched.
- `insert(index, *values)` splices `values` into the receiver before the element at `index` and returns the receiver. A negative index counts back from the end and inserts *after* that element, so `insert(-1, x)` appends; an index past the end pads the gap with `nil`. A negative index whose magnitude exceeds the length raises. Inserting no values returns the receiver unchanged.
- `clear` removes every element from the receiver in place and returns it.
- `sum` to total an array. `sum` starts from `0`; `sum(initial)` starts from `initial` (so `[1, 2, 3].sum(10)` is `16` and `["a", "b"].sum("")` is `"ab"`). A block transforms each element before it is added, so `[1, 2, 3].sum { |n| n * 2 }` is `12` and `sum(initial) { ... }` combines both. Each addition must operate on compatible operands, mirroring Ruby's `+`: summing a string with a non-string (such as the default `0` accumulator against string elements) raises rather than silently coercing the operands.
- `compact` to drop `nil` entries.
- `flatten(depth = nil)` to collapse nested arrays. No argument, `nil`, or a negative depth flattens fully; `0` returns a shallow copy; a positive depth flattens that many levels and a `Float` depth is truncated to an integer. A nonnumeric depth raises. Because a flatten's output length cannot be bounded before walking the nesting, the result is charged against the step and memory quotas as it is built, so an oversized flatten is rejected mid-build instead of after the full result materializes.
- `to_h` to build a hash from an array of two-element `[key, value]` pairs (the inverse of `Hash#to_a`). Keys use the same Ruby-style hash-key identity used everywhere else, and duplicate keys keep the last pair. A block form `to_h { |element| [key, value] }` maps each element to its pair, so the receiver's elements need not already be pairs. A non-array element, a pair that is not exactly two elements, or an unsupported key raises. In the block form the synthesized keys and values are charged against the memory quota as entries are inserted, so a block that produces fresh content per element cannot grow the result past the quota before the build completes.
- `fill(value)` / `fill(value, start, length)` / `fill(value, range)` to overwrite all or part of the receiver in place with a value, returning the receiver. A block form `fill { |index| ... }`, optionally narrowed by a `start`/`length` or range (`fill(start) { ... }`, `fill(start, length) { ... }`, `fill(range) { ... }`), computes each replacement from its index. When a block is given there is no fill-value argument: every positional argument selects the window, so `fill(0) { |i| ... }` fills from index `0` to the end rather than filling with `0`.
- `chunk(size)` to split into fixed-size slices.
- `window(size)` to build overlapping windows.
- `join(sep = "")` to produce a string. Nested arrays are joined recursively with the same separator, so `[1, [2, 3], 4].join("-")` is `"1-2-3-4"`; `nil` elements contribute an empty segment (`[1, nil, "x"].join(",")` is `"1,,x"`); and an empty array joins to `""`. The separator must be a string.

Example:

```vibe
def total_by_multiplier(values, multiplier)
  values
    .map do |value|
      value * multiplier
    end
    .sum
end
```

```vibe
[1, 2, 3, 4].filter_map { |n| if n % 2 == 0 then n * 10 end }   # [20, 40]
```

`filter_map` requires a block and takes no arguments. It calls the block once
per element and collects each result the block returns, dropping any falsy
result. Like Ruby, only `nil` and `false` are falsy, so values such as `0`,
`""`, and empty collections survive.

```vibe
[1, 2, 3, 4, 5].chunk(2)   # [[1,2], [3,4], [5]]
[1, 2, 3, 4].window(3)      # [[1,2,3], [2,3,4]]
[1, 2, 3].take(2)           # [1, 2]
[1, 2, 3].drop(1)           # [2, 3]
[1].append(2, 3)            # [1, 2, 3]
[1, 2] << 3                 # [1, 2, 3]
[3].prepend(1, 2)           # [1, 2, 3]
[3].unshift(1, 2)           # [1, 2, 3]
[1, 2, 3].shift             # 1 (receiver becomes [2, 3])
[1, 2, 3].shift(2)          # [1, 2] (receiver becomes [3])
[1, 2, 3].shift(0)          # [] (receiver unchanged)
[1, 2, 3].pop               # 3 (receiver becomes [1, 2])
[1, 2, 3].pop(2)            # [2, 3] (receiver becomes [1])
[1, 2, 2, 3].delete(2)      # 2 (receiver becomes [1, 3])
[1, 2, 3].delete(9)         # nil (receiver unchanged)
[1, 2, 3].insert(1, "x")    # [1, "x", 2, 3]
[1, 2, 3].insert(-2, "x")   # [1, 2, "x", 3]
[1].insert(3, "x")          # [1, nil, nil, "x"]
[1, 2].zip([3, 4], [5])     # [[1, 3, 5], [2, 4, nil]]
[[1, 2], [3, 4]].transpose  # [[1, 3], [2, 4]]
[[:a, 1], [:b, 2]].to_h     # { a: 1, b: 2 }
[1, 2, 3].fill(0)           # [0, 0, 0]
[1, 2, 3].fill(0, 1, 2)     # [1, 0, 0]
[1, 2, 3].fill("x", 1..2)   # [1, "x", "x"]
[1, 2, 3].fill { |i| i * 10 }    # [0, 10, 20]
[1, 2, 3].fill(0) { |i| i * 10 } # [0, 10, 20] (0 is the start index, not a value)
```

`fill` follows Ruby's indexing rules: a negative `start` counts back from the
end, a `length` that runs past the end grows the result (padding any gap with
`nil`), and a range selects the indices to replace. An explicit `length` of `0`
whose `start` is past the end still grows the array up to that start, padding the
gap with `nil` even though nothing is filled (`[1, 2, 3].fill(0, 5, 0)` is
`[1, 2, 3, nil, nil]`); a negative `length`, by contrast, is a pure no-op that
never grows the array. A `nil` `start` is read as `0` and a `nil` `length` as
omitted (filling to the end), so optional selectors held in variables that
default to `nil` behave like Ruby (`[1, 2, 3].fill(0, nil)` is `[0, 0, 0]`,
`[1, 2, 3].fill(0, 1, nil)` is `[1, 0, 0]`). The value form and the block
form are mutually exclusive: a block is never consulted when an explicit fill
value is given, and when a block is given there is no fill-value argument at all.
Every positional argument passed alongside a block selects the window, so
`fill(0) { |i| ... }` fills from index `0` to the end with block results rather
than filling with `0`. Like Ruby's `Array#fill`, it overwrites the receiver in
place and returns it.

## Mutating methods

Arrays are values (ADR-006 item 2): two variables bound to the same array each
hold their own, so neither observes the other's updates, and `equal?` reports
content equality because there is no identity left to report. The Ruby-named
mutators update the receiver they name:

- `push`/`append`, `prepend`/`unshift`, `<<`, `insert`, `fill`, and `clear`
  mutate and return the receiver.
- `pop`, `shift`, and `delete` mutate the receiver and return the removed
  value(s).
- `delete_if { |item| }` removes every element the block accepts and returns
  the receiver; `keep_if { |item| }` keeps only accepted elements. Both always
  return the receiver.
"Updates the receiver" means it updates the local, instance variable, or nested
path the call names. An array is a value: binding, passing, or returning one
produces another value, and an update through one binding is never visible
through another.

```vibe
values = [3, 1, 2, 2]
other = values
values.delete_if { |v| v == 2 }

values                  # [3, 1]
other                   # [3, 1, 2, 2] — bound before the update, unchanged by it
```

A receiver that names no such path is a temporary: the update is returned but
reaches nothing else. `cart.items.push(x)` reads the items out through an
accessor and pushes onto that value, leaving the cart's own items alone; inside
the class, `@items.push(x)` names the instance variable and updates it.

The non-mutating transforms (`map`, `sort`, `reverse`, `select`, `reject`,
`uniq`, `compact`, `+`, `first(n)`, slices, and so on) return new arrays and
never touch the receiver. The bang-named forms that only duplicated them --
`map!`, `sort!`, `reverse!`, `select!`, `reject!`, `uniq!`, `compact!` -- were
removed before 1.0; reassign the non-bang result (`values = values.sort`), or
use `keep_if` / `delete_if` where `select!` / `reject!` updated in place.

Iteration helpers walk the elements captured when iteration began: a block
that pushes to or clears the receiver mid-`each` changes the receiver, but the
in-flight iteration still visits exactly the original elements (element writes
through `arr[i] = x` remain visible). Values crossing the host boundary keep
their per-call isolation: arguments and globals are deep-cloned per call, so
in-script mutation never leaks back into the host's originals.

## Search and predicates

- `include?(value)` for membership checks.
- `index(value)` / `index { ... }` returns the first matching index, or `nil` on
  a miss. The block form returns the first index whose block result is truthy.
  `find_index` is an alias with the same value and block forms.
- `rindex(value)` / `rindex { ... }` returns the last matching index, scanning
  from the end, or `nil` on a miss.
- `count`, `count(value)`, or `count { ... }`. As in Ruby, a `value` argument
  takes precedence: `count(value) { ... }` counts elements equal to `value` and
  ignores the block.
- `values_at(*selectors)` reads several elements at once, returning a new array
  in the order the selectors were requested. An integer selector reads one
  element: negative indexes count back from the end and out-of-bounds indexes
  yield `nil`. A range selector reads a window and flattens its elements into the
  result in place, so `values_at(0..1)` is `[a[0], a[1]]` and integer and range
  selectors can be interleaved (`values_at(0..1, -1)`). A range whose end extends
  past the array pads the missing positions with `nil`, matching Ruby; a range
  whose negative start counts back before the beginning of the array raises.
  Float indexes and float range bounds truncate toward zero like Ruby (`1.9`
  reads index `1`); a non-numeric selector raises.
- `dig(*path)` for nested lookup across arrays and hashes. Each path component
  descends one level: an integer index into an array, or a symbol/string key
  into a hash, so a single `dig` can walk JSON-shaped data. Missing keys and
  out-of-range indexes (including negative indexes, which arrays treat as out of
  range) yield `nil` rather than raising. A non-integer array index raises, like
  the index operator.
- `any?`, `all?`, `none?` with an optional block or an optional `pattern`
  argument. Without arguments they test element truthiness; the block form tests
  each block result. The `pattern` form tests each element against the argument
  with case equality (`===`), as in Ruby: `any?(pattern)` is true when any
  element matches, `all?(pattern)` is true when every element matches, and
  `none?(pattern)` is true when no element matches. Range patterns therefore test
  membership rather than object identity, so `[2].any?(1..3)` is `true`;
  non-range patterns fall back to value equality and do not coerce across types.
  As with empty arrays elsewhere, `all?` and `none?` on an empty array are
  vacuously `true` and `any?` is `false`. As with `count(value)`, a `pattern`
  argument takes precedence over an attached block, which is then ignored.
- `one?` with an optional block; true only when exactly one element (or block
  result) is truthy.

```vibe
[10, 20, 30].values_at(0, -1, 9)   # [10, 30, nil]
[10, 20, 30].values_at(0..1)       # [10, 20]
[10, 20, 30].values_at(0..1, -1)   # [10, 20, 30]
[10, 20, 30].values_at(0..5)       # [10, 20, 30, nil, nil, nil]
```

`index`, `find_index`, and `rindex` accept either a value or a block, never both;
passing both raises an error. As a Vibescript extension, the value form also takes
an optional non-negative offset to start (`index`/`find_index`) or cap (`rindex`)
the search: `index(value, offset)` / `rindex(value, offset)`.

```vibe
def health_checks(values)
  {
    has_zero: values.include?(0),
    first_large_idx: values.index(100),
    first_negative_idx: values.index { |v| v < 0 },
    last_negative_idx: values.rindex { |v| v < 0 },
    all_non_negative: values.all? { |v| v >= 0 }
  }
end

[[1, 2], [3, 4]].dig(1, 0)             # 3
[{ name: "x" }, { name: "y" }].dig(1, :name) # "y"
[[1, 2]].dig(5, 0)                     # nil
```

## Indexed access

Bracket access mirrors Ruby's `Array#[]` across three selector shapes, and
`at` and `slice` are method-call spellings of the same behavior.

- `array[index]` returns the single element at `index`, counting a negative
  index back from the end. An out-of-range index returns `nil` rather than
  raising (`[10, 20, 30][-1]` is `30`, `[1][5]` is `nil`).
- `array[start, length]` returns a new subarray of up to `length` elements
  starting at `start`. A negative `start` counts back from the end. A `start`
  exactly equal to the length with a non-negative `length` yields `[]`, while a
  `start` past the length or a negative `length` returns `nil`. An oversized
  `length` is clamped to the remaining elements.
- `array[range]` returns a new subarray selected by the range bounds. Negative
  bounds count back from the end, an exclusive range drops its end, an end before
  begin yields `[]`, and a begin past the length returns `nil`.
- `at(index)` is the single-index form spelled as a method call; it agrees with
  `[index]` for every index.
- `slice(index)` mirrors `at(index)`; `slice(start, length)` and `slice(range)`
  mirror the two-argument and range bracket forms.

A negative index also works on the left of an assignment (`array[-1] = value`
updates the last element); an index outside the array raises rather than
auto-extending it.

Indexes and lengths accept `Float` values, which are truncated toward zero like
Ruby's `to_int`; any other type raises. The subarray forms always return a fresh
copy, so mutating the result never touches the original array.

```vibe
[10, 20, 30][-1]            # 30
[1][5]                      # nil
[10, 20, 30, 40][1, 2]      # [20, 30]
[10, 20, 30][3, 1]          # [] (start at the length)
[10, 20, 30][4, 1]          # nil (start past the length)
[1, 2, 3, 4][1..2]          # [2, 3]
[1, 2, 3, 4][-3..-1]        # [2, 3, 4]
[10, 20, 30].at(-1)         # 30
[10, 20, 30, 40].slice(1, 2) # [20, 30]
```

## Prefix and pattern filtering

- `take_while { ... }` keeps leading elements until the block first returns a
  falsy value, then stops. The block is never called again after the first miss.
- `drop_while { ... }` skips leading elements while the block returns truthy and
  returns the remainder, including every element after the first miss.
- `grep(pattern)` keeps elements that match `pattern` using Vibescript's
  case-equality direction (`pattern === element`), the same matcher used by
  `case`/`when`. A `Range` matches by membership; any other value matches by
  equality.
- `grep_v(pattern)` keeps the elements that do **not** match `pattern`.

Both `grep` and `grep_v` accept an optional block that transforms each kept
element before it is collected.

```vibe
[1, 2, 3, 4].take_while { |n| n < 3 }   # [1, 2]
[1, 2, 3, 4].drop_while { |n| n < 3 }   # [3, 4]
[1, 2, 3, 4].grep(2..3)                 # [2, 3]
[1, 2, 3, 4].grep_v(2..3)               # [1, 4]
[1, 2, 3, 4].grep(2..3) { |n| n * 10 }  # [20, 30]
["apple", "bee"].grep("bee")            # ["bee"]
```

Regular-expression patterns are not yet available, so `grep("e")` matches the
exact string `"e"` rather than any string containing it.

## Block iteration

These helpers yield to a block instead of building a result array. Each yielded
slice or window is an independent array, so mutating it never touches the
receiver.

- `each_slice(n)` yields non-overlapping slices of length `n`, including a
  shorter trailing slice when the length is not a multiple of `n`. `n` must be a
  positive integer. Returns `nil`.
- `each_cons(n)` yields every sliding window of length `n`; an array shorter than
  `n` yields nothing. `n` must be a positive integer. Returns `nil`.
- `reverse_each` yields values from last to first and returns the receiver.
- `each_with_index` yields each element along with its 0-based index and returns
  the receiver. It takes no arguments and requires a block.
- `cycle(n)` yields the whole array `n` times. A non-positive `n` yields nothing.
  Omitting `n` or passing `nil` cycles forever; the step quota and context
  cancellation bound the otherwise unbounded loop. Returns `nil`.

```vibe
def collect_slices(values, size)
  slices = []
  values.each_slice(size) do |slice|
    slices.push(slice)
  end
  slices
end

collect_slices([1, 2, 3, 4, 5], 2)  # [[1, 2], [3, 4], [5]]
```

```vibe
[1, 2, 3, 4].each_cons(3) do |window|
  window.sum
end                                 # yields [1, 2, 3] then [2, 3, 4]
[1, 2, 3].reverse_each do |value|
  value * 10
end                                 # yields 3, 2, 1
[1, 2].cycle(2) do |value|
  value + 1
end                                 # yields 1, 2, 1, 2
["a", "b"].each_with_index do |value, index|
  [value, index]
end                                 # yields ("a", 0) then ("b", 1)
```

## Ordering and grouping

- `reverse`, `sort`, and `sort_by`.
- `partition` to split into matching and non-matching arrays.
- `group_by` to collect values by key.
- `group_by_stable` to collect values by key while preserving group order.
- `tally` to count symbol/string occurrences.

Sorting of strings/symbols uses deterministic codepoint ordering (locale
collation is not applied).

```vibe
def summarize(players)
  grouped = players.group_by { |p| p[:status] }
  counts = players.map { |p| p[:status] }.tally

  {
    by_status: grouped,
    totals: counts
  }
end
```

## Extrema

- `min` / `max` return the smallest/largest element using the same comparison
  semantics as `sort`. They return `nil` for an empty array.
- `minmax` returns a `[min, max]` pair in one pass; an empty array yields
  `[nil, nil]`.
- `min_by { ... }` / `max_by { ... }` select the element whose block-derived key
  is smallest/largest, mirroring `sort_by`. They return `nil` for an empty
  array.

Ties resolve to the first matching element. Mixing incomparable values (for
example numbers with strings) raises an error, just like `sort`.

```vibe
def extents(scores, words)
  {
    lowest: scores.min,
    highest: scores.max,
    bounds: scores.minmax,
    shortest: words.min_by { |w| w.length },
    longest: words.max_by { |w| w.length }
  }
end
```

## Set-like Operations

Use `+` to concatenate, `-` to subtract values, and `&` to intersect:

```vibe
def unique_participants(core, late)
  (core + late).uniq.compact
end

def without_dropouts(participants, dropouts)
  participants - dropouts
end

def shared(left, right)
  left & right
end
```

`&` returns the elements common to both arrays, removing duplicates and keeping
the left array's order. Equality follows the same value semantics as `uniq`, so
nested arrays and hashes compare by content. Both operands must be arrays:

```vibe
[1, 2, 3] & [2, 3, 4]    # => [2, 3]
[1, 1, 2, 3] & [1, 3, 4] # => [1, 3]
```

The method forms `union(*others)` and `difference(*others)` accept any number of
array arguments:

- `union` concatenates the receiver with every argument array and removes
  duplicates, keeping the first occurrence of each value. Calling it with no
  arguments deduplicates the receiver. Equality follows the same value semantics
  as `uniq`, so nested arrays and hashes compare by content.
- `difference` returns the receiver's elements that do not appear in any
  argument array. Unlike `union`, it preserves duplicates within the receiver;
  only values found in the arguments are dropped.

```vibe
def attendees(core, late, extra)
  core.union(late, extra)
end

def remaining(roster, departed)
  roster.difference(departed)
end
```

```vibe
[1, 2].union([2, 3], [3, 4]) # => [1, 2, 3, 4]
[1, 1, 2, 3].difference([2])  # => [1, 1, 3]
```

Both methods return a new array and leave the receiver unchanged. A non-array
argument raises an error.

## Debug Representation

`inspect` renders an array as a parseable debug string, inspecting each element
recursively so strings keep their quotes and nested values stay distinct from
the raw interpolation rendering:

```vibe
[1, "x", nil].inspect # => "[1, \"x\", nil]"
[:a, :b].inspect      # => "[:a, :b]"
```

See [Debug Representation](stdlib_core_utilities.md#debug-representation) for the
full per-kind contract.

See `examples/arrays/` for concrete scripts exercised by the test suite.
