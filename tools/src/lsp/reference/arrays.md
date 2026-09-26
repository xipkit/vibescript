# Arrays

An `array<T>` is an ordered list of values of type `T`. A literal's element
type is the union of its elements' types, so `[1, 2]` is `array<int>` and
`[1, "a"]` is `array<int | string>`. An empty literal takes its type from a
declaration:

```vibe
players = ["alex", "maya", "li"]      # array<string>
scores: array<int> = []               # an empty literal needs a declared type
scores << 12
pair: [string, int] = ["alex", 12]    # a tuple: exactly a string, then an int
name = pair[0]                        # string
```

A tuple type `[A, B]` is an array of exactly those elements in order. Tuples
exist only at compile time; their values are ordinary arrays. Builtins use them
for fixed-length results such as `partition` and `minmax`.

Some members exist only for some element types. `sort`, `min` and `max` need a
single comparable element type (numbers, strings, symbols, times, durations or
money), `sum` without a starting value needs `int`, `compact` needs an optional
element type, and `to_h` without a block needs `[string, V]` pairs:

```vibe error=V0115
mixed = [1, "a"]
mixed.sort   # int | string is a union, so there is no single order
```

## Values, not references

Arrays are values. Binding, passing or returning an array produces another
value, so an update through one name is never visible through another. An
updating member such as `push` or `delete_if` changes the local, instance
variable or literal-key field it is called on, and returns the result:

```vibe
values = [3, 1, 2, 2]
other = values
values.delete_if { |v| v == 2 }
values   # [3, 1]
other    # [3, 1, 2, 2]

order = { id: "o-1", items: [1] }
order["items"] << 2                   # updates the field of `order`

def add(items: array<int>, item: int) -> array<int>
  items.push(item)                    # updates the parameter, not the caller's array
end

totals = [1]
more = add(totals, 2)
totals   # [1]
more     # [1, 2]
```

A receiver that names no local or field, such as the result of a call, is a
temporary: the update is returned and reaches nothing else. Every other member
returns a new array and leaves the receiver alone.

Iteration walks the elements the array had when it started: a block that
pushes to the receiver changes the receiver, but the loop still visits only the
original elements.

## Indexing

`array[index]` reads one element and has type `T?`: a negative index counts
back from the end, and an index outside the array reads `nil`.
`array[start, length]` and `array[range]` read a new subarray of type
`array<T>?`; the subarray is `[]` when `start` equals the length and `nil` when
it is past the end. `array[index] = value` replaces an element, counting a
negative index from the end, and raises for an index outside the array.

Use `fetch` when the element must be there, and test an optional element with
`!= nil` before using it:

```vibe
items = [10, 20, 30]
items[-1]          # 30
items[5]           # nil
items[1, 2]        # [20, 30]
items[1..2]        # [20, 30]
items.fetch(0) + 1 # 11

last = items[-1]
if last != nil
  puts last + 1
end
```

```vibe error=V0107
items = [10, 20, 30]
items[0] + 1   # items[0] is int?, so it must be tested or read with fetch
```

An index into a nested array is optional too, so a nested element is updated by
reading the inner array, updating it and storing it back:

```vibe
grid = [[1, 2], [3, 4]]
row = grid.fetch(0)
row[1] = 9
grid[0] = row
grid   # [[1, 9], [3, 4]]
```

### `fetch(index: int, default?: T, &block?: int -> T) -> T`

The element at `index`, counting a negative index from the end. A missing
element returns `default`, or the block's result for the index, and raises
when neither is given.

### `first -> T?` / `first(count: int) -> array<T>`

The first element, or `nil` for an empty array; with a count, the first
`count` elements, or all of them when there are fewer. A negative count raises.

### `last -> T?` / `last(count: int) -> array<T>`

The last element, or `nil` for an empty array; with a count, the last `count`
elements in order.

### `values_at(*indexes: array<int | range>) -> array<T?>`

The elements at several indexes, in the order requested. An integer counts a
negative index from the end and reads `nil` outside the array; a range adds
each element it selects, with `nil` for positions past the end, and raises when
its start is before the beginning.

### `dig(index: int, *path: array<int | string>) -> any`

Walks nested arrays and hashes: an integer indexes an array and a string reads
a hash key. A missing element or key gives `nil`. The result is `any`, so
narrow it before use.

### `sample -> T?` / `sample(count: int) -> array<T>`

A random element, or `nil` for an empty array; with a count, up to `count`
elements from distinct positions, in random order. Randomness comes from the
call's random source, which `srand` seeds.

```vibe
items = [10, 20, 30]
items.fetch(9, 0)                      # 0
items.fetch(9) { |index| index * 2 }   # 18
items.first(2)                         # [10, 20]
items.last                             # 30
items.values_at(0, -1, 9)              # [10, 30, nil]
items.values_at(0..1, -1)              # [10, 20, 30]
[[1, 2], [3, 4]].dig(1, 0)             # 3
```

## Size and search

### `length -> int`

The number of elements.

### `empty? -> bool`

Whether the array has no elements.

### `include?(value: T) -> bool`

Whether an element is equal to `value`. Equality compares contents, so nested
arrays and hashes match by value, and an int never equals a float.

### `index(value: T, offset: int = 0) -> int?` / `index(&block: T -> bool) -> int?`

The index of the first element equal to `value`, searching from `offset`, or of
the first element the block accepts; `nil` when nothing matches.

### `rindex(value: T, offset?: int) -> int?` / `rindex(&block: T -> bool) -> int?`

The index of the last element equal to `value`, searching back from `offset`,
or of the last element the block accepts; `nil` when nothing matches.

### `find(&block: T -> bool) -> T?`

The first element the block accepts, or `nil`.

### `count(value: T) -> int` / `count(&block: T -> bool) -> int`

The number of elements equal to `value`, or accepted by the block. The number
of elements is `length`.

### `any?(pattern?: T | range, &block?: T -> bool) -> bool`

Whether some element matches `pattern` or is accepted by the block. A range
pattern tests membership and any other pattern tests equality. Without a pattern
or block, it asks whether some element is neither `nil` nor `false`. An empty
array gives `false`.

### `all?(pattern?: T | range, &block?: T -> bool) -> bool`

Whether every element matches, with the same rules as `any?`. An empty array
gives `true`.

### `none?(pattern?: T | range, &block?: T -> bool) -> bool`

Whether no element matches, with the same rules as `any?`. An empty array gives
`true`.

### `one?(&block?: T -> bool) -> bool`

Whether exactly one element is accepted by the block, or, without a block, is
neither `nil` nor `false`.

```vibe
values = [4, -2, 7, -2]
values.include?(7)                  # true
values.index(-2)                    # 1
values.index(-2, 2)                 # 3
values.rindex { |v| v < 0 }         # 3
values.find { |v| v > 5 }           # 7
values.count(-2)                    # 2
values.count { |v| v > 0 }          # 2
values.any?(5..9)                   # true
values.all? { |v| v != 0 }          # true
values.none?(0)                     # true
```

## Iteration

### `each(&block: T) -> array<T>`

Runs the block for each element and returns the receiver.

### `each_with_index(&block: (T, int)) -> array<T>`

Runs the block with each element and its index, and returns the receiver.

### `reverse_each(&block: T) -> array<T>`

Runs the block for each element from last to first, and returns the receiver.

### `each_slice(size: int, &block: array<T>)`

Runs the block with consecutive slices of `size` elements; the last slice may
be shorter. `size` must be positive.

### `each_cons(size: int, &block: array<T>)`

Runs the block with each run of `size` consecutive elements; an array shorter
than `size` runs it never. `size` must be positive.

### `cycle(count: int? = nil, &block: T)`

Runs the block for every element, `count` times over; a count of zero or less
runs it never. Without a count it repeats until the block breaks, or until the
call's step quota stops it.

```vibe
slices: array<array<int>> = []
[1, 2, 3, 4, 5].each_slice(2) { |slice| slices << slice }
slices   # [[1, 2], [3, 4], [5]]

labels: array<string> = []
["a", "b"].each_with_index { |value, index| labels << "#{index}:#{value}" }
labels   # ["0:a", "1:b"]

total = 0
[1, 2, 3].reverse_each { |value| total = total * 10 + value }
total    # 321
```

## Transforming

### `map<U>(&block: T -> U) -> array<U>`

A new array of the block's results.

### `map_with_index<U>(&block: (T, int) -> U) -> array<U>`

A new array of the block's results for each element and its index.

### `flat_map<U>(&block: T -> array<U>) -> array<U>`

The block's arrays joined into one, one level deep.

### `filter_map<U>(&block: T -> U?) -> array<U>`

The block's results that are not `nil`, in one pass; a `false` result is
dropped too.

### `select(&block: T -> bool) -> array<T>`

The elements the block accepts.

### `reject(&block: T -> bool) -> array<T>`

The elements the block does not accept.

### `partition(&block: T -> bool) -> [array<T>, array<T>]`

The elements the block accepts, then the others, as a tuple of two arrays.

### `take_while(&block: T -> bool) -> array<T>`

The leading elements up to the first one the block does not accept. The block
is not called again after that element.

### `drop_while(&block: T -> bool) -> array<T>`

The elements from the first one the block does not accept onwards.

### `grep(pattern: T | range) -> array<T>` / `grep_v(pattern: T | range) -> array<T>`

The elements that match `pattern`, or with `grep_v` that do not. A range
matches by membership and any other pattern by equality.

### `drop(count: int) -> array<T>`

The elements after the first `count`. A negative count raises.

### `reverse -> array<T>`

The elements in reverse order.

### `rotate(count: int = 1) -> array<T>`

The elements rotated so that the one at `count` comes first; a negative count
rotates the other way.

### `shuffle -> array<T>`

The elements in random order, drawn from the call's random source.

### `uniq(&block?: T -> any) -> array<T>`

The elements without repeats, keeping the first of each; with a block, elements
repeat when the block gives equal results for them.

### `compact -> array<T>` on `array<T?>`

The elements that are not `nil`.

### `flatten(depth: int? = nil) -> array<any>`

Nested arrays spliced into one. Without a depth, or with a negative one, every
level is flattened; `0` copies the array and a positive depth flattens that many
levels. The result is `array<any>`, so narrow its elements before use.

### `chunk(size: int) -> array<array<T>>` / `chunk<K>(&block: T -> K) -> array<[K, array<T>]>`

Consecutive slices of `size` elements; the last may be shorter. `size` must be
positive. With a block, consecutive elements with the same key form a group,
paired with its key: a `nil` or `:_separator` key drops its element, `:_alone`
puts it in a group of its own, and other symbols starting with `_` raise.

### `window(size: int) -> array<array<T>>`

Every run of `size` consecutive elements.

### `chunk_while(&block: (T, T) -> bool) -> array<array<T>>`

The elements split into runs, keeping two neighbours together while the block
accepts them.

### `slice_when(&block: (T, T) -> bool) -> array<array<T>>`

The elements split into runs, starting a new run between two neighbours the
block accepts.

### `zip<U>(other: array<U>) -> array<[T, U?]>` / `zip<U>(first: array<U>, second: array<U>, *others: array<array<U>>) -> array<array<T | U | nil>>`

Each element paired with the other arrays' elements at its index, or with `nil`
where an array is shorter.

### `product<U>(other: array<U>) -> array<[T, U]>` / `product<U>(first: array<U>, second: array<U>, *others: array<array<U>>) -> array<array<T | U>>`

Every combination of one element from the receiver and one from each argument,
in order.

### `combination(size: int) -> array<array<T>>`

Every choice of `size` elements, in the receiver's order.

### `permutation(size?: int) -> array<array<T>>`

Every ordering of `size` elements, or of all of them.

### `repeated_combination(size: int) -> array<array<T>>`

Every choice of `size` elements where an element may be chosen more than once.

### `repeated_permutation(size: int) -> array<array<T>>`

Every ordering of `size` elements where an element may repeat.

### `transpose -> array<array<T>>` on `array<array<T>>`

Rows and columns swapped. Every row must have the same length.

### `join(separator: string = "") -> string`

The elements as text with `separator` between them. Nested arrays are joined
with the same separator and `nil` contributes an empty string.

### `to_h<V>(&block: T -> [string, V]) -> hash<string, V>` / `to_h -> hash<string, V>` on `array<[string, V]>`

A hash from the `[key, value]` pair the block returns for each element, or from
an array of pairs. A later pair replaces an earlier one with the same key.

### `to_s -> string`

The elements rendered as text, as string interpolation shows them; `inspect`
keeps quotes and is the debugging form.

```vibe
numbers = [1, 2, 3, 4]
numbers.map { |n| n * 10 }                         # [10, 20, 30, 40]
numbers.map_with_index { |n, i| n * i }            # [0, 2, 6, 12]
[[1, 2], [3]].flat_map { |row| row }               # [1, 2, 3]
numbers.filter_map { |n| n.even? ? n * 10 : nil }  # [20, 40]
evens, odds = numbers.partition { |n| n.even? }    # [2, 4] and [1, 3]
numbers.take_while { |n| n < 3 }                   # [1, 2]
numbers.grep(2..3)                                 # [2, 3]
numbers.chunk(3)                                   # [[1, 2, 3], [4]]
numbers.window(3)                                  # [[1, 2, 3], [2, 3, 4]]
[1, 2, 4, 5].slice_when { |a, b| b != a + 1 }      # [[1, 2], [4, 5]]
[1, 2].zip(["a"])                                  # [[1, "a"], [2, nil]]
[1, 2, 3].combination(2)                           # [[1, 2], [1, 3], [2, 3]]
["ab", "cd", "ax"].uniq { |s| s.chars.fetch(0) }   # ["ab", "cd"]
[1, [2, [3]]].join("-")                            # "1-2-3"
["a", "bb"].to_h { |s| [s, s.length] }             # {a: 1, bb: 2}

maybe: array<int?> = [1, nil, 2]
maybe.compact                                      # [1, 2]

pairs: array<[string, int]> = [["a", 1], ["b", 2]]
pairs.to_h                                         # {a: 1, b: 2}
```

## Folding, ordering and grouping

### `reduce(&block: (T, T) -> T) -> T?` / `reduce<A>(initial: A, &block: (A, T) -> A) -> A`

Folds the elements from the left. Without `initial` the first element starts
the fold and an empty array gives `nil`; with it, an empty array gives
`initial`.

### `sum -> T` on `array<int>` / `sum(initial: T) -> T` / `sum(&block: T -> int) -> int` / `sum<U: number | money | duration>(initial: U, &block: T -> U) -> U`

Adds the elements, or the block's values, starting from `0` or from
`initial`. An array of floats, money or durations passes its own starting value,
such as `sum(0.0)`; money of different currencies raises.

### `sort(&block?: (T, T) -> int) -> array<T>`

The elements in ascending order, or in the order the comparator block gives:
negative when its first argument comes first, zero when they tie, positive
otherwise. The sort is stable. Strings compare by code point, without locale
rules.

### `sort_by<K: comparable>(&block: T -> K) -> array<T>`

The elements ordered by the block's key, stably.

### `min -> T?` / `max -> T?` / `minmax -> [T?, T?]`

The smallest or largest element, or both as a tuple; `nil` for an empty array.
Ties resolve to the first element.

### `min_by<K: comparable>(&block: T -> K) -> T?` / `max_by<K: comparable>(&block: T -> K) -> T?`

The element with the smallest or largest key, the first on a tie, or `nil` for
an empty array.

### `group_by<K: string | symbol>(&block: T -> K) -> hash<string, array<T>>`

The elements grouped under the block's key, which is stored as a string.

### `group_by_stable<K: string | symbol>(&block: T -> K) -> array<[K, array<T>]>`

The groups as `[key, elements]` pairs, in the order each key first appears.

### `tally(&block?: T -> string | symbol) -> hash<string, int>` on `array<string>` and `array<symbol>`

How many times each element, or each key the block gives, occurs.

```vibe
[1, 2, 3].reduce { |sum, n| sum + n }              # 6
[1, 2, 3].reduce(10) { |sum, n| sum + n }          # 16
[1, 2, 3].sum                                      # 6
[1.5, 2.25].sum(0.0)                               # 3.75
[money("1.00 USD"), money("2.50 USD")].sum(money("0.00 USD"))  # 3.50 USD
[3, 1, 2].sort                                     # [1, 2, 3]
[3, 1, 2].sort { |a, b| b <=> a }                  # [3, 2, 1]
["bb", "a", "ccc"].sort_by { |w| w.length }        # ["a", "bb", "ccc"]
low, high = [3, 1, 2].minmax                       # 1 and 3
["bb", "a"].min_by { |w| w.length }                # "a"

players = [
  { name: "ada", status: "active" },
  { name: "bo", status: "away" },
  { name: "cy", status: "active" },
]
by_status = players.group_by { |player| player["status"] }
by_status.fetch("active").length                   # 2
players.map { |player| player["status"] }.tally    # {active: 2, away: 1}
```

## Updating

These members change the array they are called on (see [Values, not
references](#values-not-references)) and return the updated array or what they
removed.

### `push(*values: array<T>) -> array<T>`

Appends the values. `array << value` appends one value.

### `prepend(*values: array<T>) -> array<T>`

Inserts the values at the front, in order.

### `insert(index: int, *values: array<T>) -> array<T>`

Inserts the values before the element at `index`. A negative index counts from
the end and inserts after that element, so `insert(-1, x)` appends. An index
past the end raises, so an `array<T>` never gains elements its type excludes.

### `pop -> T?` / `pop(count: int) -> array<T>`

Removes and returns the last element, or `nil` for an empty array; with a count,
removes and returns up to `count` elements in order.

### `shift -> T?` / `shift(count: int) -> array<T>`

Removes and returns the first element, or `nil`; with a count, up to `count`
elements.

### `delete(value: T, &block?: T -> T) -> T?`

Removes every element equal to `value` and returns the last one removed. On a
miss it returns `nil`, or the block's result for `value`, and leaves the array
unchanged.

### `delete_if(&block: T -> bool) -> array<T>`

Removes the elements the block accepts.

### `keep_if(&block: T -> bool) -> array<T>`

Keeps only the elements the block accepts.

### `fill(value: T, start?: int | range, length?: int) -> array<T>`

Overwrites every element with `value`, or those from `start` (counting a
negative start from the end), `length` of them, or those the range selects.
Filling past the end raises instead of growing the array.

### `clear -> array<T>`

Removes every element.

```vibe
queue = [3]
queue.push(4, 5)       # [3, 4, 5]
queue << 6             # [3, 4, 5, 6]
queue.prepend(1, 2)    # [1, 2, 3, 4, 5, 6]
queue.insert(1, 9)     # [1, 9, 2, 3, 4, 5, 6]
queue.shift            # 1
queue.pop(2)           # [5, 6]
queue.delete(9)        # 9
queue                  # [2, 3, 4]
queue.fill(0, 1)       # [2, 0, 0]
queue.clear            # []
```

## Operators and set operations

`+` concatenates two arrays, `-` removes every element that appears in the
right operand, and `&` keeps the elements in both, without repeats and in the
left operand's order. `==` compares contents. Equality is by value throughout,
so nested arrays and hashes match by content.

### `union(*others: array<array<T>>) -> array<T>`

The receiver followed by the other arrays, without repeats, keeping the first
occurrence of each value.

### `difference(*others: array<array<T>>) -> array<T>`

The receiver's elements that appear in none of the other arrays; repeats within
the receiver stay.

```vibe
core = ["ada", "bo"]
late = ["bo", "cy"]
core + late                   # ["ada", "bo", "bo", "cy"]
core - late                   # ["ada"]
core & late                   # ["bo"]
core.union(late, ["di"])      # ["ada", "bo", "cy", "di"]
[1, 1, 2, 3].difference([2])  # [1, 1, 3]
```

## Debug representation

`inspect` renders an array so that it reads back as a literal: strings keep
their quotes and each element is inspected in turn. `to_s` and interpolation
render elements as text.

```vibe
[1, "x", nil].inspect   # "[1, \"x\", nil]"
[1, "x", nil].to_s      # "[1, x, ]"
```

## Removed spellings

These names do not compile; the compiler reports each with its replacement.

- `size` – removed; use `length`.
- `count` without an argument or block – removed; use `length`.
- `find_index` – removed; use `index`.
- `append` – removed; use `push`.
- `unshift` – removed; use `prepend`.
- `collect_concat` – removed; use `flat_map`.
- `take(count)` – removed; use `first(count)`.
- `at(index)` – removed; use `array[index]`.
- `slice(...)` – removed; use `array[start, length]` or `array[range]`.
- `reduce(:op)` and `reduce(initial, :op)` – removed; pass a block, such as
  `reduce { |sum, n| sum + n }`.
