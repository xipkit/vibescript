# Stdlib Method Reference

This page lists every builtin member of every value type, and every global
function and namespace, under its one canonical name. Each signature is written
as `vibes prelude` prints it from `src/signatures/builtins.vibe`, the table the
static checker checks every call against.

The narrative guides ([strings.md](strings.md), [arrays.md](arrays.md),
[hashes.md](hashes.md), [durations.md](durations.md), [time.md](time.md),
[builtins.md](builtins.md)) explain idioms in depth; this page favors compact
signatures and one-line descriptions.

Each section ends with the removed spellings its type still answers at runtime.
They do not compile with static types: the checker reports each one with the
rewrite shown here, and `vibes fix` applies it.

## How to Read Signatures

- `name(params) -> R` is a member or function returning `R`; without `-> R` it
  returns `nil`. A member without parameters is written, and called, without
  parentheses: `items.length`, `uuid`, `Time.now`.
- `name: T` is a required parameter, `name: T = value` an optional one with
  its default, and `name?: T` an optional one whose absence changes the
  behaviour.
- `*name: array<T>` takes any number of further arguments of type `T`.
  Parameters after a bare `*` or a `*name` are keywords, passed as
  `name: value`.
- `&block: (A, B) -> R` takes a block with parameters `A` and `B` whose value
  is `R`; `&block: A` takes a block whose value is discarded; `&block?:` makes
  the block optional.
- `T?` is `T` or `nil`; `A | B` is either; `{ name: T, other?: U }` is a shape,
  a hash with those string keys where `other` may be absent; `[A, B]` is a
  tuple, an array of exactly an `A` and then a `B`; `:ascii` is exactly that
  symbol; `number` is `int | float`; `any` is a value whose type is not known
  statically and must be narrowed; `type<T>` is a type literal such as
  `array<int>`.
- In `array<T>` members, `T` is the receiver's element type, and in
  `hash<string, V>` members `V` is its value type. `map<U>` introduces a type
  `U` inferred from the arguments and the block. A bound such as
  `T: comparable` means the member exists only when the element type is one
  type assignable to the bound, so `array<int | string>` has no `sort`.
  `comparable` is `number | string | symbol | time | duration | money`.
- A name may have several signatures, separated by ` / `. A call selects one
  by its number of positional arguments, its keyword names, and whether it
  passes a block and how many parameters the block declares, never by the
  types of its arguments.

Arrays and hashes are values. The collection mutators (array `push`, `<<`,
`prepend`, `pop`, `shift`, `insert`, `fill`, `delete`, `delete_if`, `keep_if`
and `clear`; hash `delete`, `delete_if`, `keep_if`, `clear` and `replace`)
update the local, instance variable or nested path the call names, exactly as
index assignment (`items[0] = x`, `counts["a"] = 1`) does. No other binding
sees the update, and a receiver that names no such path is a temporary whose
update is only returned. Every other member returns a new value and leaves the
receiver as it was. Strings are immutable: a bang form such as `strip!`
returns the transformed string, or `nil` when nothing changed.

```vibe
items = [3, 1, 2]
copy = items
items.push(4)
items              # [3, 1, 2, 4]
copy               # [3, 1, 2]
"  hello ".strip!  # "hello"
"hello".strip!     # nil (nothing to strip)
```

## Universal Members

Every value answers these members.

- `dup -> T` – a logical copy of the receiver. Arrays and hashes already copy
  on every binding, so it is rarely needed; a class instance keeps its
  identity, and its `dup` is the same instance.
- `inspect -> string` – the debug rendering. Unlike `to_s`, which string
  interpolation and `puts` use, it keeps quotes and escapes, so strings, arrays
  and hashes render as Vibescript literals. Hash keys render as labels
  (`name:`, or `"with space":`), entries follow insertion order, and cycles
  render as `<cycle>`. The rendered length is charged to the memory quota
  before the string is built.
- `is_type?(type: symbol) -> bool` – tests the value's type without converting
  it. The symbol names a builtin type (`:int`, `:float`, `:number`, `:string`,
  `:bool`, `:symbol`, `:nil`, `:duration`, `:time`, `:money`, `:array`,
  `:hash`, `:range`), a class or an enum by its exact name, and a trailing `?`
  also accepts `nil` (`:int?`). In a condition it narrows a local of type `any`
  or of a union, like a nil test.

| Value | `to_s` (interpolation, `puts`) | `inspect` |
| --- | --- | --- |
| `nil` | (empty) | `nil` |
| `true` | `true` | `true` |
| `42` | `42` | `42` |
| `:ok` | `ok` | `:ok` |
| `"a\nb"` | `a`, then `b` on the next line | `"a\nb"` |
| `[1, "x", nil]` | `[1, x, ]` | `[1, "x", nil]` |
| `{ a: 1, b: "x" }` | (no `to_s`) | `{a: 1, b: "x"}` |

```vibe
"a\nb".inspect            # "\"a\\nb\""
[1, "x", nil].inspect     # "[1, \"x\", nil]"
{ a: 1, b: "x" }.inspect  # "{a: 1, b: \"x\"}"
:ok.inspect               # ":ok"
1.is_type?(:number)       # true
"5".is_type?(:int)        # false: the test never converts

count: int? = nil
count.is_type?(:int?)     # true
```

### Checked casts

`value.as(T)` checks a value against any type an annotation can name, at
runtime and exactly as a typed parameter does, and gives the expression that
type. It narrows a value of type `any`, such as a `JSON.parse` result, and a
declared union. A mismatch raises the same boundary error as a typed
parameter. `JSON.parse_as(text, T)` parses and casts in one step.

```vibe
raw = JSON.parse("{\"id\": 7, \"tags\": [\"a\"]}")
record = raw.as(hash<string, any>)
id = record.fetch("id").as(int)
tags = record.fetch("tags").as(array<string>)
id + tags.length  # 8

label: int | string = "draft"
label.as(string).upcase  # "DRAFT"
```

### Removed spellings

- `itself` – returns the receiver unchanged; removed, write the receiver
  itself.
- `tap` – removed; run the block's statements, then use the receiver.
- `yield_self` – removed; bind the receiver to a local and write the block's
  expression.
- `eql?` / `equal?` – removed; use `==`, which compares values.
- `nil?` – removed; write `value == nil`, which also narrows `value`.
- `clone` – removed; use `dup`.
- `freeze` – removed; every value is already immutable or a value, so write
  the receiver itself.
- `frozen?` – removed; it answered `true` for every value.
- `string` – removed; use `to_s`.
- `is_a?` / `kind_of?` / `instance_of?` – removed; use `is_type?(:Name)`.
- `send` / `public_send` – removed; call the member directly, or use `case`
  over the name.
- `respond_to?` – removed; use `case` over the name, or `is_type?`.

## Strings

See [strings.md](strings.md) for worked examples. Indexes and lengths count
characters, not bytes, unless noted. `+` concatenates two strings, `*`
repeats one (`"ab" * 2` is `"abab"`), and `%` formats an array of values as
`format` does (`"%s:%03d" % ["id", 7]` is `"id:007"`). `text[i]`,
`text[start, length]` and `text[range]` read characters as `string?`.

### Inspecting

- `length -> int` – the number of characters.
- `bytesize -> int` – the number of bytes.
- `empty? -> bool` – whether the string has no characters.
- `ord -> int` – the code point of the first character; raises on an empty
  string.
- `chr -> string` – the first character, or `""` for an empty string.
- `getbyte(index: int) -> int?` – the byte (`0..255`) at a byte offset;
  negative offsets count from the end, and an offset out of range gives `nil`.
- `byteslice(start: int | range, length?: int) -> string?` – bytes by byte
  offset, returned verbatim; `nil` when the start is out of range or the length
  is negative.
- `bytes -> array<int>` – the bytes, one entry per byte of a multibyte
  character.
- `codepoints -> array<int>` – the code points, one per character.
- `chars -> array<string>` – the characters.
- `lines -> array<string>` – the lines, each keeping its `"\n"`; a `"\r\n"`
  ending stays attached.
- `hex -> int` – the leading hexadecimal digits (optional whitespace, sign and
  `0x` prefix, `_` separators); `0` when none lead.
- `oct -> int` – the leading digits in the base a `0x`, `0b`, `0o` or `0d`
  prefix selects, octal by default; `0` when none lead.
- `between?(min: string, max: string) -> bool` – whether the string sorts
  between the bounds, inclusive, by code point.
- `clamp(min: string?, max: string?) -> string` – the string bounded by code
  point order; `nil` leaves a side open.
- `casecmp(other: string) -> int` – `-1`, `0` or `1`, comparing ASCII letters
  without case.
- `casecmp?(other: string) -> bool` – equality under Unicode simple case
  folding.
- `to_s -> string` – the string itself.

### Converting

- `to_i -> int` – the base-10 integer the string spells, ignoring surrounding
  whitespace; raises unless the whole string is an integer.
- `to_f -> float` – the finite float the string spells; raises otherwise.
- `to_sym -> symbol` – the symbol with this name; any contents are accepted.

```vibe
"42".to_i   # 42
"3.5".to_f  # 3.5
"ff".hex    # 255
"héllo".length    # 5
"héllo".bytesize  # 6
```

### Searching and matching

- `start_with?(prefix: string, *prefixes: array<string>) -> bool` – whether
  the string begins with any of the prefixes.
- `end_with?(suffix: string, *suffixes: array<string>) -> bool` – whether the
  string ends with any of the suffixes.
- `include?(text: string) -> bool` – whether `text` occurs anywhere.
- `index(text: string, offset: int = 0) -> int?` – the first character index
  of `text` at or after `offset`; a negative offset counts from the end.
- `rindex(text: string, offset?: int) -> int?` – the last character index of
  `text` at or before `offset`, the end by default.
- `count(set: string, *sets: array<string>) -> int` – the number of characters
  in every set, where a set lists characters, ranges such as `a-z` and a
  leading `^` for the complement.
- `match(pattern: string | regex, offset: int = 0) -> match_data?` – the first
  match at or after the character `offset`, or `nil`; a string pattern is a
  regular expression.
- `match?(pattern: string | regex, offset: int = 0) -> bool` – whether the
  pattern matches, without building match data.
- `scan(pattern: string | regex) -> array<string | array<string?>>` – every
  non-overlapping match; each is the matched string, or the array of its
  groups when the pattern has groups.

`match`, `match?` and `scan` compile a string pattern as a regular expression
in RE2 syntax and enforce the [guard limits](#guard-limits).

```vibe
"2024-03-05".match?("[0-9]+")          # true
"a1b22".scan("[0-9]+")                 # ["1", "22"]
"a1b22".scan("([a-z])([0-9]+)")        # [["a", "1"], ["b", "22"]]
found = "2024-03-05".match("([0-9]+)-([0-9]+)")
if found != nil
  found.captures                       # ["2024", "03"]
end
```

### Slicing and building

- `slice(start: int | range | string, length?: int) -> string?` – the
  characters at an index, from an index for a length, in a range, or the text
  itself when it occurs; `nil` when out of range or absent.
- `concat(*texts: array<string>) -> string` – the string with the texts
  appended.
- `prepend(*texts: array<string>) -> string` – the string with the texts
  prepended, in order.
- `insert(index: int, text: string) -> string` – the string with `text`
  inserted before the character at `index`; a negative index inserts after
  the character it selects, so `-1` appends. An index out of range raises.
- `center(width: int, pad: string = " ") -> string` – padded on both sides to
  `width` characters, the extra character on the right.
- `ljust(width: int, pad: string = " ") -> string` – padded on the right.
- `rjust(width: int, pad: string = " ") -> string` – padded on the left.

A width at or below the length returns the string unchanged; the pad must not
be empty and is repeated, then cut at a character boundary.

### Case and order

- `upcase(mode?: :ascii) -> string` – uppercased with full Unicode case
  mapping, so `"Straße".upcase` is `"STRASSE"`; `:ascii` maps only ASCII
  letters.
- `downcase(mode?: :ascii | :fold) -> string` – lowercased with full Unicode
  mapping; `:fold` applies Unicode case folding, so `"Straße".downcase(:fold)`
  is `"strasse"`.
- `capitalize(mode?: :ascii) -> string` – the first character titlecased and
  the rest lowercased.
- `swapcase(mode?: :ascii) -> string` – every cased character flipped.
- `reverse -> string` – the characters in reverse order.

### Trimming and character sets

- `strip -> string` – leading and trailing ASCII whitespace and NUL removed;
  Unicode spaces such as NBSP stay.
- `lstrip -> string` – leading whitespace removed.
- `rstrip -> string` – trailing whitespace removed.
- `squish -> string` – both ends trimmed and every inner run of whitespace,
  Unicode included, collapsed to one space.
- `chomp(separator?: string?) -> string` – one trailing `"\r\n"`, `"\n"` or
  `"\r"` removed; with a separator, that suffix once; with `""`, every trailing
  newline.
- `chop -> string` – the last character removed, or a trailing `"\r\n"`.
- `delete_prefix(prefix: string) -> string` – `prefix` removed when present.
- `delete_suffix(suffix: string) -> string` – `suffix` removed when present.
- `delete(set: string, *sets: array<string>) -> string` – the characters in
  every set removed.
- `squeeze(*sets: array<string>) -> string` – runs of the same character
  collapsed, only for characters in the sets when given.
- `tr(from: string, to: string) -> string` – each character of `from`
  replaced by the one at its position in `to`; both take ranges, and a leading
  `^` in `from` complements it.

### Replacing, splitting and templating

- `sub(pattern: string | regex, replacement: string) -> string` /
  `sub(pattern: string | regex, &block: string -> string) -> string` – the
  first match replaced by `replacement` or by the block's result for the
  matched text. A string pattern matches literally.
- `gsub(pattern: string | regex, replacement: string) -> string` /
  `gsub(pattern: string | regex, &block: string -> string) -> string` – every
  match replaced.
- `split(separator: string? = nil, limit: int = 0) -> array<string>` – fields
  split on runs of ASCII whitespace, or on `separator`; `""` splits into
  characters. A positive `limit` caps the number of fields, `0` drops trailing
  empty fields and a negative limit keeps them.
- `partition(separator: string) -> [string, string, string]` – the text
  before the first `separator`, the separator and the text after.
- `rpartition(separator: string) -> [string, string, string]` – the same
  around the last `separator`.
- `template(context: hash<string, any>, *, strict: bool = false) -> string` –
  `{{name}}` and `{{user.name}}` placeholders filled from `context`; a missing
  value stays as written, or raises with `strict: true`.

With a regex pattern, `sub` and `gsub` expand `\1` to `\9`, `\0` or `\&` (the
whole match), `` \` `` and `\'` (the text before and after), `\+` (the last
group that matched), `\k<name>` and `\\` in `replacement`; `$1` is literal
text. The regex [guard limits](#guard-limits) apply.

```vibe
"a-b-a".sub("a", "x")                    # "x-b-a"
"a1b2".gsub(Regex.new("[0-9]"), "#")     # "a#b#"
"a1b2".gsub(Regex.new("[0-9]")) { |digit| (digit.to_i * 2).to_s }  # "a2b4"
"a,b,,c".split(",")                      # ["a", "b", "", "c"]
"a=b=c".rpartition("=")                  # ["a=b", "=", "c"]
"Hi {{user.name}}".template({ user: { name: "Ada" } })  # "Hi Ada"
```

### Iterating

- `each_char(&block: string) -> string` – yields each character; returns the
  string.
- `each_byte(&block: int) -> string` – yields each byte.
- `each_codepoint(&block: int) -> string` – yields each code point.
- `each_line(&block: string) -> string` – yields each line with its newline.

### Bang variants

Each of these returns the transformed string, or `nil` when nothing changed:

- `strip!`, `lstrip!`, `rstrip!`, `squish!`, `chomp!`, `chop!`
- `delete!`, `delete_prefix!`, `delete_suffix!`, `tr!`, `squeeze!`
- `upcase!`, `downcase!`, `capitalize!`, `swapcase!`, `reverse!`

`sub!` and `gsub!` return the rewritten string whenever the pattern matched,
even when the replacement reproduces the text, and `nil` only when it never
matched.

### Removed spellings

- `size` – removed; use `length`, the number of characters.
- `intern` – removed; use `to_sym`.
- `clear` – removed; write `""`.
- `replace` – removed; write the replacement string itself.

The `regex:` keyword of `sub`, `gsub`, `sub!` and `gsub!` is removed as well:
pass `Regex.new(pattern)` for a regular expression and a string for literal
text.

## Symbols

Symbols name enum members, and a symbol literal naming a member is accepted
wherever that enum is expected. Hash keys are strings, not symbols.

- `to_s -> string` – the symbol's name.
- `to_sym -> symbol` – the symbol itself.

```vibe
:draft.to_s      # "draft"
"draft".to_sym   # :draft
:draft == "draft"  # false
```

### Removed spellings

- `id2name` – removed; use `to_s`.

## Arrays

See [arrays.md](arrays.md) for worked examples. An array literal has the
union of its elements' types, so `[1, 2]` is `array<int>`; an empty literal
needs a declared type (`names: array<string> = []`). `items[i]` is `T?`, with
negative indexes counting from the end, and `items[start, length]` and
`items[range]` are `array<T>`; `fetch` returns `T` or raises. `items[i] = v`
writes an existing index and raises past the end. `+` concatenates, `-`
removes every element equal to one in the right operand, and `items << value`
appends one element to the named array.

### Reading

- `length -> int` – the number of elements.
- `empty? -> bool` – whether the array has no elements.
- `first -> T?` / `first(count: int) -> array<T>` – the first element, or the
  first `count` elements.
- `last -> T?` / `last(count: int) -> array<T>` – the last element, or the last
  `count` elements.
- `fetch(index: int, default?: T, &block?: int -> T) -> T` – the element at
  `index`, counting back from the end when negative; out of bounds, the block's
  value for the index, else `default`, else an error.
- `dig(index: int, *path: array<int | string>) -> any` – the value down a path
  of array indexes and hash keys, or `nil` when a step is missing.
- `values_at(*indexes: array<int | range>) -> array<T?>` – the elements at the
  indexes and ranges, `nil` where out of bounds.
- `sample -> T?` / `sample(count: int) -> array<T>` – a random element, or up to
  `count` distinct ones.
- `include?(value: T) -> bool` – whether an element equals `value`.
- `index(value: T, offset: int = 0) -> int?` / `index(&block: T -> bool) -> int?`
  – the first index of `value` at or after `offset`, or the first index the
  block accepts.
- `rindex(value: T, offset?: int) -> int?` / `rindex(&block: T -> bool) -> int?`
  – the last index of `value` at or before `offset`, or the last index the
  block accepts.
- `count(value: T) -> int` / `count(&block: T -> bool) -> int` – the number of
  elements equal to `value`, or accepted by the block. Without an argument or
  block, write `length`.
- `all?(pattern?: T | range, &block?: T -> bool) -> bool` – whether every
  element matches the pattern (a range tests membership) or the block.
- `any?(pattern?: T | range, &block?: T -> bool) -> bool` – whether some
  element matches.
- `none?(pattern?: T | range, &block?: T -> bool) -> bool` – whether no element
  matches.
- `one?(&block?: T -> bool) -> bool` – whether exactly one element is accepted.
- `to_s -> string` – the display rendering, as `puts` prints it.

### Iterating

- `each(&block: T) -> array<T>` – yields each element; returns the array.
- `each_with_index(&block: (T, int)) -> array<T>` – yields each element and
  its index.
- `each_slice(size: int, &block: array<T>)` – yields consecutive slices of
  `size` elements, the last possibly shorter.
- `each_cons(size: int, &block: array<T>)` – yields each run of `size`
  consecutive elements.
- `reverse_each(&block: T) -> array<T>` – yields from last to first.
- `cycle(count: int? = nil, &block: T)` – yields every element `count` times,
  or until the block breaks when `count` is `nil`.

### Transforming

- `map<U>(&block: T -> U) -> array<U>` – the block's value for each element.
- `map_with_index<U>(&block: (T, int) -> U) -> array<U>` – the block's value
  for each element and its index.
- `flat_map<U>(&block: T -> array<U>) -> array<U>` – the block's arrays
  concatenated.
- `filter_map<U>(&block: T -> U?) -> array<U>` – the block's values, without
  the `nil` and `false` ones.
- `select(&block: T -> bool) -> array<T>` – the elements the block accepts.
- `reject(&block: T -> bool) -> array<T>` – the elements the block rejects.
- `find(&block: T -> bool) -> T?` – the first element the block accepts.
- `partition(&block: T -> bool) -> [array<T>, array<T>]` – the accepted
  elements, then the others.
- `take_while(&block: T -> bool) -> array<T>` – the leading elements the block
  accepts.
- `drop_while(&block: T -> bool) -> array<T>` – the elements after that
  leading run.
- `drop(count: int) -> array<T>` – the elements after the first `count`.
- `grep(pattern: T | range) -> array<T>` – the elements equal to `pattern`, or
  in the range.
- `grep_v(pattern: T | range) -> array<T>` – the other elements.
- `uniq(&block?: T -> any) -> array<T>` – the first of each group of equal
  elements, or of elements with equal block values.
- `compact -> array<T>` – on `array<T?>`, the elements that are not `nil`.
- `flatten(depth: int? = nil) -> array<any>` – nested arrays collapsed
  completely, or `depth` levels.
- `reverse -> array<T>` – the elements in reverse order.
- `rotate(count: int = 1) -> array<T>` – the elements rotated left by
  `count`, right when negative.
- `shuffle -> array<T>` – the elements in random order.
- `chunk(size: int) -> array<array<T>>` – consecutive slices of `size`
  elements.
- `window(size: int) -> array<array<T>>` – every run of `size` consecutive
  elements.
- `chunk_while(&block: (T, T) -> bool) -> array<array<T>>` – runs whose
  adjacent pairs the block accepts.
- `slice_when(&block: (T, T) -> bool) -> array<array<T>>` – runs split where
  the block accepts an adjacent pair.
- `zip<U>(other: array<U>) -> array<[T, U?]>` /
  `zip<U>(first: array<U>, second: array<U>, *others: array<array<U>>) -> array<array<T | U | nil>>`
  – each element paired with the other arrays' elements at its index, `nil`
  where they are shorter.
- `product<U>(other: array<U>) -> array<[T, U]>` /
  `product<U>(first: array<U>, second: array<U>, *others: array<array<U>>) -> array<array<T | U>>`
  – every combination of one element from each array.
- `combination(size: int) -> array<array<T>>` – every choice of `size`
  elements, in order.
- `permutation(size?: int) -> array<array<T>>` – every ordering of `size`
  elements, all of them by default.
- `repeated_combination(size: int) -> array<array<T>>` – combinations that may
  repeat an element.
- `repeated_permutation(size: int) -> array<array<T>>` – permutations that may
  repeat an element.
- `transpose -> array<array<T>>` – on `array<array<T>>`, rows and columns
  swapped; raises when the rows differ in length.
- `union(*others: array<array<T>>) -> array<T>` – the distinct elements of all
  the arrays.
- `difference(*others: array<array<T>>) -> array<T>` – the elements in none of
  the others.
- `join(separator: string = "") -> string` – the elements' `to_s` renderings
  joined, nested arrays included.
- `to_h<V>(&block: T -> [string, V]) -> hash<string, V>` / `to_h -> hash<string, V>`
  – a hash from the `[key, value]` pair the block returns for each element, or,
  on `array<[string, V]>`, from the pairs themselves; a later duplicate key wins.

```vibe
[1, 2, 3, 4].filter_map { |n| n.odd? ? n * 10 : nil }  # [10, 30]
[1, 2, 3, 4].partition { |n| n > 2 }                   # [[3, 4], [1, 2]]
[1, 2].zip([3])                                        # [[1, 3], [2, nil]]
["a", "bb"].to_h { |s| [s, s.length] }                 # {a: 1, bb: 2}
pairs: array<[string, int]> = [["a", 1], ["b", 2]]
pairs.to_h                                             # {a: 1, b: 2}
```

### Folding, ordering and grouping

- `reduce(&block: (T, T) -> T) -> T?` /
  `reduce<A>(initial: A, &block: (A, T) -> A) -> A` – the elements folded from
  the first, `nil` for an empty array, or from `initial`.
- `sum -> T` / `sum(initial: T) -> T` / `sum(&block: T -> int) -> int` /
  `sum<U: number | money | duration>(initial: U, &block: T -> U) -> U` – the
  total: of an `array<int>` from `0`, of numbers, money or durations from
  `initial`, or of the block's values.
- `sort(&block?: (T, T) -> int) -> array<T>` – when `T` is comparable, the
  elements in ascending order, or by a comparator block returning a negative,
  zero or positive int; the sort is stable.
- `sort_by<K: comparable>(&block: T -> K) -> array<T>` – the elements ordered
  by the block's key, stably.
- `min -> T?` / `max -> T?` – when `T` is comparable, the smallest or largest
  element.
- `minmax -> [T?, T?]` – both in one pass.
- `min_by<K: comparable>(&block: T -> K) -> T?` /
  `max_by<K: comparable>(&block: T -> K) -> T?` – the element with the smallest
  or largest key; ties go to the first.
- `group_by<K: string | symbol>(&block: T -> K) -> hash<string, array<T>>` –
  the elements grouped under the block's key.
- `group_by_stable<K: string | symbol>(&block: T -> K) -> array<[K, array<T>]>`
  – the groups as `[key, elements]` pairs, in the order each key first appears.
- `tally(&block?: T -> string | symbol) -> hash<string, int>` – on arrays of
  strings or symbols, how often each element, or each block key, occurs.

Strings and symbols order by code point, without locale collation.

```vibe
[5, 1, 4].sort { |a, b| b <=> a }          # [5, 4, 1]
["bb", "a"].sort_by { |s| s.length }        # ["a", "bb"]
[1, 2, 3].reduce(10) { |acc, n| acc + n }  # 16
[1.5, 2.25].sum(0.0)                        # 3.75
["a", "b", "a"].tally                       # {a: 2, b: 1}
```

### Updating the array

- `push(*values: array<T>) -> array<T>` – appends the values; returns the
  array.
- `prepend(*values: array<T>) -> array<T>` – inserts the values at the front,
  in order.
- `pop -> T?` / `pop(count: int) -> array<T>` – removes and returns the last
  element, or the last `count` elements in order.
- `shift -> T?` / `shift(count: int) -> array<T>` – removes and returns the
  first element, or the first `count`.
- `insert(index: int, *values: array<T>) -> array<T>` – inserts the values
  before `index`; a negative index inserts after the element it selects, so
  `-1` appends. An index past the end raises.
- `fill(value: T, start?: int | range, length?: int) -> array<T>` – overwrites
  every element, those from `start` for `length`, or those in the range, with
  `value`; it never grows the array and raises past the end.
- `delete(value: T, &block?: T -> T) -> T?` – removes every element equal to
  `value` and returns the last removed, or `nil` (the block's value) on a miss.
- `delete_if(&block: T -> bool) -> array<T>` – removes the elements the block
  accepts.
- `keep_if(&block: T -> bool) -> array<T>` – keeps only the elements the block
  accepts.
- `clear -> array<T>` – removes every element.

```vibe
items = [1, 2, 3, 4, 5]
items.pop       # 5
items.shift(2)  # [1, 2]
items           # [3, 4]
items.prepend(1, 2)
items.delete(4) # 4
items           # [1, 2, 3]
```

### Removed spellings

- `size` – removed; use `length`, the number of elements.
- `find_index` – removed; use `index`.
- `append` – removed; use `push`.
- `unshift` – removed; use `prepend`.
- `collect_concat` – removed; use `flat_map`.
- `take` – removed; use `first(count)`.
- `at` – removed; index the array, as in `items[index]`.
- `slice` – removed; index the array, as in `items[start, length]` or
  `items[range]`.

`reduce(:+)` and other operation shorthands are removed as well: pass a block,
as in `reduce { |acc, n| acc + n }`.

## Hashes

See [hashes.md](hashes.md) for worked examples. Hash keys are strings, and a
`name:` label in a literal is the string key `"name"`. A literal is a shape,
a record with those fields: `{ name: "Ada", age: 36 }` is
`{ name: string, age: int }`, and `record["name"]` is a `string`. A
dictionary is declared as `hash<string, V>` (`counts: hash<string, int> = {}`),
and `counts[key]` is `V?`; a shape whose fields all have type `V` is assignable
to it. The members below are those of `hash<string, V>`. Entries keep
insertion order in `keys`, `values` and every iteration.

### Reading

- `length -> int` – the number of entries.
- `empty? -> bool` – whether the hash has no entries.
- `key?(key: string) -> bool` – whether `key` is present.
- `value?(value: V) -> bool` – whether some value equals `value`.
- `keys -> array<string>` – the keys in insertion order.
- `values -> array<V>` – the values in insertion order.
- `fetch(key: string, default?: V, &block?: string -> V) -> V` – the value for
  `key`; when missing, the block's value for the key, else `default`, else an
  error.
- `fetch_values(*keys: array<string>, &block?: string -> V) -> array<V>` – the
  values for the keys, in order; a missing key takes the block's value or
  raises.
- `values_at(*keys: array<string>) -> array<V?>` – the values for the keys,
  `nil` where missing.
- `dig(key: string, *path: array<string | int>) -> any` – the value down a path
  of hash keys and array indexes, or `nil` when a step is missing.

### Iterating

- `each(&block: (string, V)) -> hash<string, V>` /
  `each(&block: [string, V]) -> hash<string, V>` – yields each key and value,
  or each `[key, value]` pair to a one-parameter block; returns the hash.
- `each_key(&block: string) -> hash<string, V>` – yields each key.
- `each_value(&block: V) -> hash<string, V>` – yields each value.
- `each_with_index(&block: ([string, V], int)) -> hash<string, V>` – yields
  each pair and its index.
- `map<U>(&block: (string, V) -> U) -> array<U>` /
  `map<U>(&block: [string, V] -> U) -> array<U>` – the block's value for each
  entry.
- `map_with_index<U>(&block: ([string, V], int) -> U) -> array<U>` – the
  block's value for each pair and its index.
- `to_a -> array<[string, V]>` – the `[key, value]` pairs.
- `flatten(depth: int = 1) -> array<any>` – the pairs flattened into
  `[key, value, ...]`, or deeper.

### Transforming and filtering

- `select(&block: (string, V) -> bool) -> hash<string, V>` – the entries the
  block accepts.
- `reject(&block: (string, V) -> bool) -> hash<string, V>` – the entries the
  block rejects.
- `slice(*keys: array<string>) -> hash<string, V>` – only the listed keys that
  are present.
- `except(*keys: array<string>) -> hash<string, V>` – every entry but the
  listed keys.
- `merge(*others: array<hash<string, V>>, &block?: (string, V, V) -> V) -> hash<string, V>`
  – the entries of every hash, later ones winning, or the block's value for a
  key present in both.
- `transform_keys(&block: string -> string | symbol) -> hash<string, V>` –
  each key replaced by the block's value.
- `deep_transform_keys(&block: string -> string | symbol) -> hash<string, V>`
  – the same, through nested hashes and arrays.
- `remap_keys(mapping: hash<string, string | symbol>) -> hash<string, V>` –
  keys renamed by `mapping`; unmapped keys stay.
- `transform_values<U>(&block: V -> U) -> hash<string, U>` – each value
  replaced by the block's value.
- `compact -> hash<string, V>` – on `hash<string, V?>`, the entries whose value
  is not `nil`.

### Updating the hash

- `delete(key: string, &block?: string -> V) -> V?` – removes the entry and
  returns its value, or `nil` (the block's value) when missing.
- `delete_if(&block: (string, V) -> bool) -> hash<string, V>` – removes the
  entries the block accepts.
- `keep_if(&block: (string, V) -> bool) -> hash<string, V>` – keeps only the
  entries the block accepts.
- `clear -> hash<string, V>` – removes every entry.
- `replace(other: hash<string, V>) -> hash<string, V>` – replaces every entry
  with those of `other`.

```vibe
scores: hash<string, int> = { ada: 3, bo: 5 }
scores.fetch("ada")                        # 3
scores.fetch("cy", 0)                      # 0
scores["cy"]                               # nil
scores.select { |name, score| score > 3 }  # {bo: 5}
scores.merge({ ada: 10 }) { |key, old, new| old + new }  # {ada: 13, bo: 5}
scores["cy"] = 1
scores.keys                                # ["ada", "bo", "cy"]
```

### Removed spellings

- `size` – removed; use `length`, the number of entries.
- `has_key?` / `member?` / `include?` – removed; use `key?`.
- `has_value?` – removed; use `value?`.
- `store` – removed; assign by index, as in `counts[key] = value`.

## Integers

Integers have arbitrary precision: arithmetic, comparison, conversion and JSON
continue past the 64-bit range, and results that fit return to compact
storage. Indexes, counts, range endpoints, the iteration members and money,
duration and time arithmetic stay within 64 bits and raise beyond them. `//`
is floor division and `%` the floored remainder, so `-7 // 2` is `-4` and
`-7 % 3` is `2`. `/` on two integers is true division, returning a float; until
the switchover the checker rejects it (V0109) and asks for `//`. Integer
division or remainder by zero raises.

- `abs -> int` – the absolute value.
- `between?(min: number, max: number) -> bool` – whether the integer lies
  between the bounds, inclusive.
- `clamp(bounds: range) -> int` / `clamp(min: int?, max: int?) -> int` – the
  integer bounded by a range or by two bounds; `nil` leaves a side open.
- `even? -> bool` / `odd? -> bool` – parity.
- `zero? -> bool` / `positive? -> bool` / `negative? -> bool` – sign tests.
- `nonzero? -> int?` – the integer, or `nil` when it is `0`.
- `succ -> int` / `pred -> int` – the next or previous integer.
- `round(digits: int = 0) -> int` – unchanged for non-negative `digits`; a
  negative `digits` rounds half away from zero to that power of ten, so
  `1234.round(-2)` is `1200`.
- `floor(digits: int = 0) -> int` – like `round`, toward negative infinity.
- `ceil(digits: int = 0) -> int` – like `round`, toward positive infinity.
- `div(divisor: number) -> int` – the floored quotient; `-5.div(2)` is `-3`.
- `divmod(divisor: int) -> [int, int]` – the floored quotient and remainder.
- `fdiv(divisor: number) -> float` – float division; a zero divisor gives an
  infinity or NaN.
- `remainder(divisor: int) -> int` – the remainder with the receiver's sign
  (truncated division).
- `times(&block: int) -> int` – yields `0` to `n - 1`; returns the integer.
- `upto(limit: int, &block: int) -> int` – yields each integer up to `limit`.
- `downto(limit: int, &block: int) -> int` – yields each integer down to
  `limit`.
- `step(limit: int, by: int = 1, &block: int) -> int` – yields every `by`-th
  integer until it passes `limit`; `by` must not be `0`.
- `seconds -> duration` / `minutes -> duration` / `hours -> duration` /
  `days -> duration` / `weeks -> duration` – a duration of that many units.
- `to_i -> int` – the integer itself.
- `to_f -> float` – the integer as a float.
- `to_s -> string` – the decimal digits.

```vibe
7.divmod(2)     # [3, 1]
7 // 2          # 3
7.fdiv(2)       # 3.5
5.clamp(1, 3)   # 3
1234.round(-2)  # 1200
90.minutes.to_i # 5400
```

### Removed spellings

- `second` / `minute` / `hour` / `day` / `week` – removed; use the plural
  unit, such as `1.seconds`.
- `next` – removed; use `succ`.
- `modulo` – removed; use the `%` operator.

## Floats

Floats are IEEE 754 doubles. `1.0 / 0` is `Infinity` and `0.0 / 0.0` is
`NaN`; comparisons involving `NaN` are false, and `NaN == NaN` is false.
`to_s` and `inspect` print the shortest decimal form, in exponent notation
for very large or small magnitudes, and drop a trailing `.0`, so `4.0.to_s` is
`"4"`. `JSON.stringify`
rejects non-finite floats, and converting one to an integer raises.

- `abs -> float` – the absolute value.
- `between?(min: number, max: number) -> bool` – whether the float lies
  between the bounds, inclusive.
- `clamp(bounds: range) -> number` / `clamp(min: float?, max: float?) -> float`
  – the float bounded by a range or by two bounds; `nil` leaves a side open.
- `round -> int` / `round(digits: int) -> number` – rounded half away from
  zero to an int, or to `digits` fractional digits as a float when `digits` is
  positive (`1.234.round(2)` is `1.23`).
- `floor -> int` / `floor(digits: int) -> number` – like `round`, toward
  negative infinity.
- `ceil -> int` / `ceil(digits: int) -> number` – like `round`, toward positive
  infinity.
- `zero? -> bool` / `positive? -> bool` / `negative? -> bool` – sign tests.
- `nonzero? -> float?` – the float, or `nil` when it is zero.
- `nan? -> bool` – whether the float is NaN.
- `infinite? -> int?` – `1` or `-1` for an infinity, otherwise `nil`.
- `finite? -> bool` – whether the float is neither infinite nor NaN.
- `div(divisor: number) -> int` – the floored quotient as an int.
- `divmod(divisor: number) -> [int, float]` – the floored quotient and the
  remainder with the divisor's sign.
- `fdiv(divisor: number) -> float` – float division.
- `remainder(divisor: number) -> float` – the remainder with the receiver's
  sign.
- `to_i -> int` – truncated toward zero; exact beyond 64 bits.
- `to_f -> float` – the float itself.
- `to_s -> string` – the display text.

```vibe
1.234.round(2)   # 1.23
2.5.round        # 3
3.7.floor        # 3
7.5.divmod(2)    # [3, 1.5]
nan = 0.0 / 0.0
nan.nan?         # true
(1.0 / 0).infinite?  # 1
```

### Removed spellings

- `modulo` – removed; use the `%` operator.

## Money

Money values come from `money("12.50 USD")` and `money_cents(1250, "USD")`.
They add and subtract in the same currency, multiply and divide by integers
(division truncates), and compare with `<`, `>` and `==`.

- `cents -> int` – the amount in minor units.
- `currency -> string` – the three-letter currency code.
- `between?(min: money, max: money) -> bool` – whether the amount lies between
  the bounds, inclusive.
- `to_s -> string` – the amount and currency, as `"100.50 USD"`.

```vibe
price = money("100.50 USD")
price.cents                    # 10050
price.currency                 # "USD"
(price + money("1.00 USD")).to_s  # "101.50 USD"
```

### Removed spellings

- `amount` – removed; use `to_s`.
- `format` – removed; use `to_s`.

## Durations

See [durations.md](durations.md) for arithmetic and worked examples.
Durations are whole seconds; they come from integer units such as `90.minutes`
and from `Duration.build` and `Duration.parse`.

- `weeks -> int` / `days -> int` / `hours -> int` / `minutes -> int` – the
  whole number of units, truncated toward zero.
- `to_i -> int` – the total seconds.
- `in_seconds -> float` / `in_minutes -> float` / `in_hours -> float` /
  `in_days -> float` / `in_weeks -> float` – the length in that unit, with
  its fraction.
- `in_months -> float` / `in_years -> float` – approximations using 30-day
  months and 365-day years.
- `parts -> { days: int, hours: int, minutes: int, seconds: int }` – the
  length broken into parts.
- `iso8601 -> string` – the ISO 8601 form, such as `"PT1H30M"`.
- `to_s -> string` – the seconds, such as `"5400s"`.
- `between?(min: duration, max: duration) -> bool` – whether the length lies
  between the bounds, inclusive.
- `ago -> time` / `from_now -> time` – the time this long before or after now.
- `before(start: time) -> time` / `after(start: time) -> time` – the time this
  long before or after `start`.

```vibe
shift = 90.minutes
shift.minutes     # 90
shift.hours       # 1
shift.in_hours    # 1.5
shift.parts       # {days: 0, hours: 1, minutes: 30, seconds: 0}
shift.iso8601     # "PT1H30M"
5.minutes.before(Time.utc(2024, 1, 1)).iso8601  # "2023-12-31T23:55:00Z"
```

### Removed spellings

- `second` / `seconds` – removed; use `to_i`.
- `minute` / `hour` / `day` / `week` – removed; use the plural unit, such as
  `minutes`.
- `format` – removed; use `to_s`.
- `since` – removed; use `from_now`, or `after(start)`.
- `until` – removed; use `ago`, or `before(start)`.

`ago` and `from_now` count from now and take no argument, while `before` and
`after` take the time to count from: `ago(start)` is now `before(start)`, and
`after` without a time is now `from_now`.

## Times

See [time.md](time.md) for construction, zones and layouts. `time + duration`
and `time - duration` shift a time, `time + n` and `time - n` shift it by
seconds, and `time - time` is the float number of seconds between them. Times
compare with `<`, `>`, `==` and `<=>`.

- `year -> int` / `month -> int` / `day -> int` – the calendar date.
- `hour -> int` / `min -> int` / `sec -> int` – the time of day.
- `usec -> int` / `nsec -> int` – the fraction of the second in micro- or
  nanoseconds.
- `subsec -> float` – the fraction of the second.
- `wday -> int` – the day of the week, `0` for Sunday.
- `yday -> int` – the day of the year, from `1`.
- `zone -> string` – the zone abbreviation, such as `"UTC"`.
- `utc_offset -> int` – the offset from UTC in seconds.
- `utc? -> bool` – whether the time is in UTC.
- `dst? -> bool` – whether daylight saving time is in effect.
- `sunday? -> bool` / `monday? -> bool` / `tuesday? -> bool` /
  `wednesday? -> bool` / `thursday? -> bool` / `friday? -> bool` /
  `saturday? -> bool` – day-of-week tests.
- `between?(min: time, max: time) -> bool` – whether the time lies between the
  bounds, inclusive.
- `to_i -> int` – seconds since the Unix epoch.
- `to_f -> float` – seconds since the epoch, with the fraction.
- `to_a -> [int, int, int, int, int, int, int, int, bool, string]` – `[sec,
  min, hour, day, month, year, wday, yday, dst?, zone]`.
- `to_s -> string` – the RFC 3339 form with nanoseconds when present.
- `iso8601(digits: int = 0) -> string` – the RFC 3339 form with `digits`
  fractional digits, truncated; at most 100.
- `httpdate -> string` – the HTTP date in GMT, such as
  `"Tue, 02 Jan 2024 03:04:05 GMT"`.
- `rfc2822 -> string` – the mail date with the time's offset; a UTC time uses
  `-0000`.
- `format(layout: string) -> string` – formatted with a Go layout such as
  `"2006-01-02"`.
- `strftime(format: string) -> string` – formatted with a percent pattern such
  as `"%Y-%m-%d"`; output is capped at 1 MiB.
- `utc -> time` – the same instant in UTC.
- `localtime(zone: string? = nil) -> time` – the same instant in a zone, such
  as `"America/New_York"` or `"+05:30"`, or in the host's zone.
- `round(digits: int = 0) -> time` – rounded half away from zero to `digits`
  fractional digits.
- `floor -> time` / `ceil -> time` – truncated or rounded up to the second.

```vibe
t = Time.utc(2024, 1, 2, 3, 4, 5)
t.iso8601                # "2024-01-02T03:04:05Z"
t.iso8601(3)             # "2024-01-02T03:04:05.000Z"
t.format("2006-01-02")   # "2024-01-02"
t.strftime("%H:%M")      # "03:04"
t.tuesday?               # true
t.localtime("+05:30").hour  # 8
(t + 1.days).day         # 3
```

### Removed spellings

- `mon` – removed; use `month`.
- `mday` – removed; use `day`.
- `tv_sec` – removed; use `to_i`.
- `tv_usec` – removed; use `usec`.
- `tv_nsec` – removed; use `nsec`.
- `to_r` – removed; use `to_f`.
- `hash` – removed; use `to_i * 1000000000 + nsec`.
- `gmt_offset` / `gmtoff` – removed; use `utc_offset`.
- `gmtime` / `getutc` / `getgm` – removed; use `utc`.
- `gmt?` – removed; use `utc?`.
- `isdst` – removed; use `dst?`.
- `xmlschema` / `rfc3339` – removed; use `iso8601`.
- `rfc822` – removed; use `rfc2822`.
- `getlocal` – removed; use `localtime`.

`time.<=>(other)` is removed too; write `time <=> other`.

## Ranges

Ranges are integer ranges, inclusive (`1..5`) or exclusive (`1...5`). A
descending range such as `5..1` iterates downward. `for` loops and `case`
branches accept ranges, and each iteration step is charged to the step quota,
so a wide range fails on the quota rather than running unbounded.

- `include?(value: number) -> bool` – whether `value` lies within the range,
  honouring an exclusive end.
- `first -> int` / `first(count: int) -> array<int>` – the start, or the first
  `count` integers.
- `last -> int` / `last(count: int) -> array<int>` – the end, even when the
  range excludes it, or the last `count` integers.
- `length -> int` – the number of integers the range iterates over.
- `exclude_end? -> bool` – whether the range is written with `...`.
- `min -> int?` / `max -> int?` – the smallest or largest integer, or `nil` for
  an empty range.
- `each(&block: int) -> range` – yields each integer; returns the range.
- `step(by: int, &block: int) -> range` – yields every `by`-th integer from
  the start; `by` must be positive.
- `map<U>(&block: int -> U) -> array<U>` – the block's value for each integer.
- `select(&block: int -> bool) -> array<int>` /
  `reject(&block: int -> bool) -> array<int>` – the integers the block accepts,
  or rejects.
- `find(&block: int -> bool) -> int?` – the first integer the block accepts.
- `count(&block: int -> bool) -> int` – how many integers the block accepts.
- `reduce(&block: (int, int) -> int) -> int?` /
  `reduce<A>(initial: A, &block: (A, int) -> A) -> A` – the integers folded.
- `sum(initial: int = 0) -> int` – the total plus `initial`.
- `to_a -> array<int>` – every integer, in iteration order.
- `to_s -> string` – the range as written, such as `"1..5"`.

```vibe
range = 1..5
range.include?(3)                  # true
range.length                       # 5
range.select { |n| n.odd? }        # [1, 3, 5]
range.reduce(10) { |acc, n| acc + n }  # 25
(5..1).to_a                        # [5, 4, 3, 2, 1]
```

### Removed spellings

- `size` – removed; use `length`, the number of integers in the range.
- `member?` / `cover?` – removed; use `include?`.

`count` without a block is removed as well; use `length`.

## Nil and Booleans

- `nil.to_s -> string` – the empty string.
- `bool.to_s -> string` – `"true"` or `"false"`.

A condition takes a `bool` and nothing else: test an optional value with
`value == nil` or `value != nil`, which narrows it in the branch.

## Regexes

A regex literal `/pattern/flags` or `Regex.new(pattern)` makes a regex value.
Patterns use RE2 syntax; the flags are `i` (ignore case) and `m` (`.` matches
newlines). `text =~ regex` is the character index of the first match or `nil`,
`text !~ regex` whether it does not match, and a regex in a `when` clause
matches strings.

- `regex.match(text: string) -> match_data?` – the first match, or `nil`.
- `regex.match?(text: string) -> bool` – whether the regex matches.
- `regex.source -> string` – the pattern text.
- `regex.flags -> string` – the flags, such as `"i"`.

```vibe
pattern = /id-([0-9]+)/i
pattern.match?("ID-12")  # true
pattern.source           # "id-([0-9]+)"
"ID-12" =~ pattern       # 0
```

## Match Data

A successful match (type `match_data`) comes from `match` on a string or a
regex. It is read through these members and by index, `found[0]` for the whole
match and `found[1]` for the first group, and it cannot be modified.

- `to_s -> string` – the whole match.
- `captures -> array<string?>` – the groups, `nil` for one that did not take
  part.
- `named_captures -> hash<string, string?>` – the named groups.
- `pre_match -> string` / `post_match -> string` – the text before and after
  the match.
- `begin(group: int) -> int?` / `end(group: int) -> int?` – the character
  offsets of a group.

## Enum Values

`enum Status` declares a type whose members are read as `Status::Draft`. A
symbol literal naming a member, such as `:draft`, is accepted wherever a
`Status` is expected.

- `name -> string` – on a member, its name, such as `"Draft"`; on the enum,
  the enum's name.
- `symbol -> symbol` – the member's symbol, such as `:draft`.
- `enum -> enum_type` – the member's enum.
- `to_s -> string` – the name.

## Rescued Errors

`rescue => error` binds a value of type `error`.

- `message -> string` – the error's message.
- `class -> string` – the error class, such as `"RuntimeError"`.
- `backtrace -> array<string>` – the script call trace.
- `code_frame -> string` – the source snippet at the failure.

`error.type` is removed in favour of `class`, and `error.to_s` in favour of
`message`.

## Builtin Functions

Global functions and namespaces available in every script. See
[builtins.md](builtins.md) for narrative examples.

### Global functions

- `assert(condition: bool, message?: string)` – raises an assertion error with
  `message` when `condition` is false.
- `format(pattern: string, *values: array<any>) -> string` – the values
  formatted by percent directives, as in `format("%.2f", 1.234)`; `text % values`
  does the same.
- `puts(*values: array<any>)` – writes each value's `to_s` on its own line.
- `print(*values: array<any>)` – writes the values without newlines.
- `p` / `p<T>(value: T) -> T` /
  `p(first: any, second: any, *rest: array<any>) -> array<any>` – writes each
  value inspected and returns `nil`, the value, or the values.
- `warn(*values: array<any>)` – writes each value on its own line to the
  error output.
- `loop(&block: ()) -> any` – runs the block until it breaks; the value of
  `break value` is the result.
- `money(amount: string) -> money` – parses an amount and currency, such as
  `"12.50 USD"`.
- `money_cents(cents: int, currency: string) -> money` – money from minor
  units.
- `rand -> float` / `rand(max: int | range) -> int` – a float in `[0, 1)`, or an
  int below `max` or in the range.
- `srand(seed: int? = nil) -> int?` – seeds this call's `rand` sequence and
  returns the previous seed.
- `random_id(length: int = 16) -> string` – an alphanumeric token of `length`
  characters, at most 1024.
- `uuid -> string` – a version 7 UUID.
- `to_int(value: number | string) -> int` – an int, an integral float or a
  base-10 integer string as an int; raises otherwise.
- `to_float(value: number | string) -> float` – a number or a finite numeric
  string as a float; raises otherwise.
- `require(path: string, *, as: string? = nil) -> any` – loads a module and
  returns its exports; both arguments are string literals. See
  [builtins.md](builtins.md#module-loading).

### JSON

- `JSON.parse(text: string) -> any` – the value the JSON text describes:
  hashes, arrays, strings, ints, floats, bools and `nil`. Integers of any size
  parse exactly. The result is `any` and must be narrowed.
- `JSON.parse_as<T>(text: string, schema: type<T>) -> T` – parses and checks
  the result against a type literal, such as `{ name: string, age?: int }`;
  a mismatch raises the boundary error.
- `JSON.stringify(value: any) -> string` – the JSON text; symbols and enum
  members become strings, and cycles and non-finite floats raise.

Both directions enforce a 1 MiB payload limit and reject more than 10,000
nested arrays and objects.

### Math

- `Math::PI -> float` / `Math::E -> float` – the constants.
- `Math.sqrt(x: number) -> float` / `Math.cbrt(x: number) -> float` – square
  and cube roots.
- `Math.sin(x: number) -> float` / `Math.cos(x: number) -> float` /
  `Math.tan(x: number) -> float` – trigonometry in radians.
- `Math.asin(x: number) -> float` / `Math.acos(x: number) -> float` /
  `Math.atan(x: number) -> float` / `Math.atan2(y: number, x: number) -> float`
  – inverse trigonometry.
- `Math.exp(x: number) -> float` – `E` to the power `x`.
- `Math.log(x: number, base?: number) -> float` /
  `Math.log2(x: number) -> float` / `Math.log10(x: number) -> float` –
  logarithms.
- `Math.hypot(x: number, y: number) -> float` – the hypotenuse, without
  intermediate overflow.

An argument outside a function's domain, such as `Math.sqrt(-1)`, raises a
domain error. `Math.log(0)` is `-Infinity`, and NaN propagates.

```vibe
Math.sqrt(2)      # 1.4142135623730951
Math.hypot(3, 4)  # 5.0, printed as 5
Math.log(8, 2)    # 3.0, printed as 3
Math::PI          # 3.141592653589793
```

### Regex

- `Regex.new(pattern: string) -> regex` – compiles a pattern.
- `Regex.escape(text: string) -> string` – `text` with every metacharacter
  escaped.
- `Regex.union(*patterns: array<string>) -> regex` – a regex matching any of
  the patterns.
- `Regex.match(pattern: string, text: string) -> string?` – the first match in
  `text`.
- `Regex.replace(text: string, pattern: string, replacement: string) -> string`
  – the first match replaced; `replacement` expands `$1`.
- `Regex.replace_all(text: string, pattern: string, replacement: string) -> string`
  – every match replaced.

`Regex.match` takes the pattern first, while the replace helpers take the text
first.

```vibe
Regex.match("ID-[0-9]+", "ID-12 ID-34")        # "ID-12"
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1")  # "X-12"
Regex.escape("a.b")                            # "a\\.b"
```

### Duration

- `Duration.build(*, weeks: number = 0, days: number = 0, hours: number = 0, minutes: number = 0, seconds: number = 0) -> duration`
  – a duration from named parts; at least one is required.
- `Duration.parse(text: string) -> duration` – parses Go (`"1h30m"`) or
  ISO 8601 (`"PT1H30M"`, `"P2W"`) durations.

```vibe
Duration.build(hours: 1, minutes: 30).to_i  # 5400
Duration.parse("P2W").days                  # 14
```

### Time

Zones are IANA names (`"America/New_York"`), `"UTC"`, `"LOCAL"` or offsets
such as `"+05:30"`.

- `Time.now(*, in: string? = nil) -> time` – the current time.
- `Time.utc(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0) -> time`
  – a calendar time in UTC.
- `Time.local(year: int, month: int = 1, day: int = 1, hour: int = 0, min: int = 0, sec: int = 0, usec: number = 0, *, in: string? = nil) -> time`
  – a calendar time in the host's zone, or in `in:`.
- `Time.at(seconds: number, subsec?: number, unit?: :microsecond | :millisecond | :nanosecond, *, in: string? = nil) -> time`
  – the time a number of epoch seconds after 1970; `subsec` is in
  microseconds unless `unit` says otherwise.
- `Time.parse(text: string, layout: string? = nil, *, in: string? = nil) -> time`
  – parses RFC 3339, RFC 1123, `YYYY-MM-DD[ HH:MM:SS]`, `YYYY/MM/DD` and
  `MM/DD/YYYY` forms, or `text` in a Go layout.

```vibe
Time.utc(2024).iso8601                           # "2024-01-01T00:00:00Z"
Time.parse("2024-01-02", "2006-01-02").day       # 2
Time.at(1, 500, :millisecond).nsec               # 500000000
Time.local(2024, 3, 1, in: "UTC").month          # 3
```

### Removed spellings

- `sprintf` – removed; use `format`.
- `now` – removed; use `Time.now`, and `Time.now.iso8601` for the string.
- `Hash.new` – removed; write `{}` with a declared type, such as
  `counts: hash<string, int> = {}`.
- `Regexp.new` / `Regexp.union` / `Regexp.escape` – removed; use `Regex.new`,
  `Regex.union` and `Regex.escape`.
- `Regexp.quote` – removed; use `Regex.escape`.
- `Regexp.last_match` – removed; it always answered `nil`. Keep the result of
  `match` instead.
- `Time.gm` – removed; use `Time.utc`.
- `Time.mktime` / `Time.new` – removed; use `Time.local`, which takes the zone
  as `in:`.
- `Duration.build(seconds)` – the positional form is removed; write
  `Duration.build(seconds: n)`.
- `assert(condition, message: text)` – the keyword form is removed; pass the
  message as the second argument.

## Guard Limits

JSON, regex, formatting and ID helpers enforce fixed limits so hostile data
cannot exhaust host memory or CPU. They are not configurable, and exceeding one
raises a `LimitError` naming the guard.

| Guard | Limit |
| --- | --- |
| `JSON.parse` input and `JSON.stringify` output | 1 MiB |
| `JSON.parse` and `JSON.stringify` nesting | 10,000 arrays and objects |
| `format`, `text % values` and `time.strftime` output | 1 MiB |
| Regex pattern (`Regex.*`, regex literals, and string `match`, `match?`, `scan`, `sub`, `gsub`) | 16 KiB |
| Regex text, replacement and output | 1 MiB |
| `scan` match-index table, worst case | 256 MiB |
| `random_id` length | 1024 characters |
| Each value written by `puts`, `print`, `warn` and `p` | 1 MiB |
