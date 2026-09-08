# Stdlib Method Reference

This page is the canonical reference for every built-in method and global
function the interpreter ships. Each entry is derived from the runtime member
dispatch tables, so the listing is complete: a method appears here if and only
if the interpreter implements it.

The narrative guides ([strings.md](strings.md), [arrays.md](arrays.md),
[hashes.md](hashes.md), [durations.md](durations.md), [time.md](time.md),
[builtins.md](builtins.md)) explain idioms and patterns in depth; this page
favors compact signatures and one-line descriptions.

## How to Read Signatures

- `method(arg, optional = default, keyword: default) -> return_type` describes
  positional arguments, defaults, and keyword arguments.
- `{ |item| }` marks a method that takes a block, written
  `do |item| ... end` in Vibescript.
- `a | b` in a return type means the method returns either type.
- Collection methods with Ruby mutator names update their receiver: the array
  splicers (`push`/`append`, `prepend`/`unshift`, `<<`, `insert`, `fill`,
  `clear`), the removers (`pop`, `shift`, `delete`, `delete_if`/`keep_if`), and
  the hash mutators (`store`, `delete`, `clear`, `delete_if`/`keep_if`,
  `replace`). What they update is the local, instance variable, or nested path
  the call names, exactly as index assignment does (`arr[0] = x`,
  `hash[:k] = v`). Arrays and hashes are values, so no other binding sees the
  update, and a receiver that names no such path is a temporary whose update is
  returned and reaches nothing else. Non-mutator names (`map`, `select`, `sort`,
  `merge`, `+`, ...) return a new value and leave the receiver untouched.
- Strings are immutable values: string bang variants (`strip!`, `gsub!`, ...)
  return the transformed string — or `nil` when nothing changed — rather than
  rewriting the receiver in place.

```vibe
"hello".strip!    # nil (nothing to strip)
"  hello ".strip! # "hello"
```

## Universal Members

These members are available on every value, regardless of kind.

- `itself -> self` – returns the receiver unchanged. Useful in pipelines and
  block-based callbacks where an identity step keeps the call shape uniform. It
  takes no arguments; passing any positional or keyword argument is an error.
  A class that defines its own `itself` method overrides this builtin, matching
  Ruby's method-resolution order.

```vibe
"x".itself     # "x"
3.itself       # 3
[1, 2].itself  # [1, 2]
nil.itself     # nil
```

## Universal Predicates

Every value answers two equality predicates in addition to the `==` operator.
They report `false` rather than raising when the kinds differ, and a class may
override them with its own methods of the same name.

- `eql?(other) -> bool` – hash-key equality. True only when both operands share
  a kind and compare equal, so `1.eql?(1.0)` is `false` even though both are
  numerically one. Composites (arrays, hashes) compare by content.
- `equal?(other) -> bool` – identity for values that have it, content equality
  for collections. Immutable scalars (`nil`, `bool`, `int`, `float`, `string`,
  `symbol`, money, duration, time, range) are identical when they share a kind
  and value, so `1.equal?(1)` is `true`. Arrays and hashes are values: `equal?`
  asks about contents, the same question as `==`, because collections carry no
  identity. Script instances, enums, and enum values are identical only when
  they refer to the same backing object. Two enum values that compare `==` (and
  `eql?`) because they share an owner and member name are still not `equal?`
  when they hold distinct storage — for example, a value cloned out to the host
  and handed back by a capability.

```vibe
1 == 1        # true
1.eql?(1)     # true
1.eql?(1.0)   # false (Int never eql-matches a Float)
1.equal?(1)   # true

a = [1, 2, 3]
b = a
c = [1, 2, 3]
a.eql?(c)     # true  (same contents)
a.equal?(b)   # true  (same contents; collections have no identity)
a.equal?(c)   # true  (same contents)
[].equal?([]) # true
{}.equal?({}) # true
a[0] = 9
a.equal?(b)   # false (b is still [1, 2, 3])
```

Binding one collection to two names still produces two values. They compare
`equal?` when their contents match, and an update through one binding is never
visible through the other.

A NaN float is `equal?` to itself even though `==` never holds for NaN. Because
floats carry no distinct identity, any two NaN floats are also `equal?`, keeping
the predicate reflexive (`x = 0.0 / 0.0; x.equal?(x)` is `true`).

## Debug Representation

Every core value kind responds to `inspect`, returning a `string` debug
rendering. Unlike the output rendering used by string interpolation (which is
the `to_s` form), `inspect` keeps quotes and escaping so the result is
unambiguous; for strings, arrays, and hashes the rendering also parses back as a
Vibescript literal.

- `inspect -> string` – available on `nil`, booleans, integers, floats,
  strings, symbols, arrays, and hashes (`inspect` takes no arguments and no
  block). Namespace and host objects share the hash member methods, so `inspect`
  renders their fields with the same hash form.

| Kind | `to_s` (interpolated) | `inspect` |
| --- | --- | --- |
| `nil` | (empty) | `nil` |
| `true` | `true` | `true` |
| `42` | `42` | `42` |
| `:ok` | `ok` | `:ok` |
| `"a\nb"` | `a` then `b` on the next line | `"a\nb"` |
| `[1, "x", nil]` | `[1, x, ]` | `[1, "x", nil]` |
| `{ a: 1, b: "x" }` | `{a: 1, b: x}` | `{a: 1, b: "x"}` |

```vibe
"a\nb".inspect       # "\"a\\nb\""  (the six characters: " a \ n b ")
[1, "x", nil].inspect # "[1, \"x\", nil]"
{ a: 1, b: "x" }.inspect # "{a: 1, b: \"x\"}"
:ok.inspect          # ":ok"
nil.inspect          # "nil"
```

Strings are double-quoted and escape `\\`, `\"`, `\n`, `\t`, and the
interpolation marker `\#{`; any other byte is written verbatim, since
Vibescript's double-quoted literals have no `\r`/`\xNN`/`\uNNNN` escapes (so the
rendering stays a parseable literal rather than emitting an escape the language
cannot decode). Hash keys render in Vibescript's colon-label form (`name:`, or
`"with space":` when the key is not a bare identifier) rather than Ruby's
unsupported hash-rocket syntax, so an inspected hash parses back as a Vibescript
literal. Inspected entries follow the hash's insertion order, so the rendering
is stable across calls and matches iteration. Symbols render as `:name`, or as
`:"name"` (Ruby's shape) when the name is not a bare identifier — the quoted form
is a debug rendering for symbols (such as those created from a quoted hash key)
that have no bare-symbol literal syntax, not a re-parseable literal. Cycles
render as `<cycle>`. The rendered length is charged against the sandbox memory
quota before the string is built, so inspecting a huge composite fails with a
quota error instead of allocating an oversized result.

## Universal Methods

Every value responds to `nil?`, including script class instances, classes,
and enum values:

- `nil? -> bool` – `true` only for `nil`, `false` for every other value
  (Ruby's `Object#nil?`). Takes no arguments. It resolves through the same
  central fallback as the [object helpers](#object-helpers) below, so a
  user-defined method named `nil?` keeps precedence.

The scalar kinds whose display form is bounded by their own footprint (`nil`,
booleans, integers, floats, strings, symbols, money, durations, and times) also
respond to `to_s` and `string`:

- `to_s -> string` – the value's display rendering (the `to_s` form used by
  string interpolation). For strings it returns the receiver; for symbols,
  durations, and times it matches the kind-specific `to_s` documented below.
- `string -> string` – alias for `to_s`; the documented Vibescript conversion
  idiom used in [typing.md](typing.md) (for example `id.string`).

Both take no arguments. Arrays, hashes, and ranges deliberately do **not**
respond to `to_s`/`string`: their rendering can be arbitrarily large, so they
expose only `inspect`, which projects the rendered length against the sandbox
memory quota before allocating. Strings additionally parse numeric text with
`to_i`/`to_f` (see [Conversion](#conversion)), and integers and floats convert
between numeric kinds with `to_i`/`to_f` (see [Integers](#integers) and
[Floats](#floats)).

```vibe
42.nil?     # false
nil.nil?    # true
[1, 2].nil? # false
42.string   # "42"
:ok.to_s    # "ok"
```

## Object Helpers

Every core value kind responds to the block-yielding helpers `tap` and
`yield_self`. Both require a block, take no positional or keyword arguments, and
pass the receiver as the block's single argument. They differ only in what they
return:

- `tap { |value| } -> receiver` – yields the receiver, ignores the block's
  result, and returns the receiver. Use it to thread a side effect (such as
  logging) through a pipeline without changing the value.
- `yield_self { |value| } -> block result` – yields the receiver and returns the
  block's result, so it rewrites a value inline.

```vibe
"ada".tap { |name| name.upcase }        # "ada" (block result discarded)
"ada".yield_self { |name| name.upcase } # "ADA"
3.yield_self { |n| n * 100 }            # 300
```

These helpers resolve only when the receiver does not already define a member of
the same name, so a hash key, instance variable, or user-defined method named
`tap` or `yield_self` keeps precedence.

## Object Introspection

Every value kind responds to the Ruby-style introspection predicates. They are
resolved the way `Object`'s methods are in Ruby: available on any receiver, but a
script class may define its own method of the same name and that definition wins.
Unlike the data-eligible block helpers `tap`/`yield_self`, a data slot keyed with
a predicate name (a hash key, instance variable, or class variable named
`respond_to?`, `is_a?`, and so on) is data, not a method, so it never shadows the
predicate — `{ "respond_to?": 1 }.respond_to?(:keys)` still calls the predicate.

- `respond_to?(name) -> bool` (optionally `respond_to?(name, include_all)`) –
  reports whether the receiver has a callable member named `name` (a symbol or a
  string). Data — hash keys, namespace constants, and instance variables — is not
  a method and reports `false`; namespace functions such as `Math.sqrt` report
  `true`. Private methods report `false` unless the predicate is reached through
  implicit receiver dispatch or `include_all` is `true`, matching
  `respond_to?`'s privacy rules.
- `is_a?(class) -> bool` and `kind_of?(class) -> bool` – report whether the
  receiver is an instance of the given script class. There is no inheritance
  and no module membership, so these test direct class identity and agree with
  `instance_of?` on every value; a module argument always reports `false`. A
  non-instance receiver (a core value, a class value, an enum value) reports
  `false`. The argument must be a class.
- `instance_of?(class) -> bool` – reports whether the receiver is an instance of
  exactly the given script class.
- `is_type?(atom) -> bool` – tests the receiver against a type atom without
  coercion. The atom is a symbol or string naming a primitive (`:int`,
  `:float`, `:number`, `:string`, `:bool`, `:symbol`, `:nil`, `:duration`,
  `:time`, `:money`), a bare container (`:array`, `:hash`/`:object`, `:range`),
  or a class or enum name matched by exact name. A module alias
  qualifies a name the way
  annotations do: `v.is_type?("lv.Level")` tests against the enum the
  required module exports, and an unknown qualified name is an error. A trailing `?` tests the nullable form:
  `'int?'` is int or nil. Parameterized spellings such as `array<int>` and
  unknown lowercase names are errors. `"5".is_type?(:int)` is `false` — the
  test never converts.

```vibe
"Ada".respond_to?(:length)   # true
"Ada".respond_to?(:nope)     # false
{ name: "x" }.respond_to?(:name) # false  (a data key, not a method)
{ name: "x" }.respond_to?(:keys) # true

class User
end

user = User.new
user.is_a?(User)        # true
user.kind_of?(User)     # true
user.instance_of?(User) # true
42.is_a?(User)          # false

1.is_type?(:int)        # true
1.is_type?(:number)     # true
"5".is_type?(:int)      # false (no coercion)
nil.is_type?('int?')    # true
user.is_type?(:User)    # true (exact name)
```

## Strings

See [strings.md](strings.md) for worked examples. Indexes and lengths count
Unicode characters, not bytes, unless noted.

### Inspection

- `size -> int` – number of characters.
- `length -> int` – alias for `size`.
- `bytesize -> int` – number of UTF-8 bytes.
- `empty? -> bool` – true when the string has no characters.
- `ord -> int` – codepoint of the first character; errors on an empty string.
- `chr -> string` – first character, or an empty string for an empty receiver.
- `getbyte(index) -> int | nil` – byte at a byte offset (`0..255`); negative
  offsets count from the end, and an out-of-range offset returns `nil`.
- `byteslice(index) | byteslice(start, length) | byteslice(range) -> string |
  nil` – substring by byte offset; negative offsets count from the end, an
  out-of-range start or negative length returns `nil`, and bytes are returned
  verbatim without UTF-8 normalization.
- `clamp(min, max) -> string` – receiver bounded by lexicographic string
  comparison; `nil` leaves one side open.
- `hex -> int` – leading characters parsed as a hexadecimal integer (optional
  whitespace, sign, `0x` prefix, and underscore separators); `0` when no hex
  digit leads, and an `integer out of range` error past the `int64` bounds.
- `oct -> int` – leading characters parsed using a base inferred from a
  `0x`/`0b`/`0o`/`0d` prefix (octal by default); same lenient parsing,
  zero-on-failure, and `int64` overflow behavior as `hex`.
- `inspect -> string` – double-quoted, escaped debug rendering (see
  [Debug Representation](#debug-representation)).

### Conversion

- `to_sym -> symbol` – the symbol named by the string. Any contents are
  accepted verbatim, including whitespace, punctuation, and the empty string.
- `intern -> symbol` – alias for `to_sym`.
- `to_s -> string` – the receiver itself (Ruby's `String#to_s`).
- `string -> string` – alias for `to_s`; the documented Vibescript conversion
  idiom (see [typing.md](typing.md)).
- `to_i -> int` – parse a base-10 integer. Unlike Ruby's lenient `String#to_i`
  (which ignores trailing characters and returns `0` on failure), this is strict
  like the global `to_int`: surrounding whitespace is trimmed, but an empty or
  non-integer string raises rather than silently yielding `0`.
- `to_f -> float` – parse a finite float with the same strict semantics as the
  global `to_float`; an empty, non-numeric, or non-finite string raises.

```vibe
"42".to_i  # 42
"3.5".to_f # 3.5
"id".string # "id"
```

### Search and Matching

- `start_with?(*prefixes) -> bool` – true when the string begins with any of
  the given prefixes. Candidates are checked left to right and matching
  short-circuits, so a non-string is only rejected if reached before a match.
- `end_with?(*suffixes) -> bool` – true when the string ends with any of the
  given suffixes, with the same left-to-right short-circuit behavior.
- `include?(substring) -> bool` – true when `substring` occurs anywhere.
- `index(substring, offset = 0) -> int | nil` – first character index at or
  after `offset`; `nil` when not found. A negative `offset` counts back from the
  end (`size + offset`) and yields `nil` when it falls before the start.
- `rindex(substring, offset = size) -> int | nil` – last character index at or
  before `offset`; `nil` when not found. A negative `offset` counts back from the
  end (`size + offset`) and yields `nil` when it falls before the start.
- `match(pattern) -> array | nil` – regex match returning
  `[full, capture1, ...]` (unmatched groups are `nil`); `nil` when no match.
  Given a block, yields the match data and returns the block's result, or `nil`
  without invoking the block when there is no match.
- `match?(pattern, offset = 0) -> bool` – allocation-light predicate returning
  `true` when `pattern` matches at or after the character `offset`, else
  `false`. Anchors keep the full-string context across the offset; an offset
  past the end yields `false`, and negative offsets are rejected.
- `scan(pattern) -> array` – every non-overlapping regex match. With no capture
  groups the result is an array of full match strings; with one or more groups
  each match contributes a nested array of its captured substrings (`nil` for an
  optional group that did not participate), mirroring Ruby. Given a block, yields
  each match (using the same per-match shape) and returns the receiver string.

`match`, `match?`, and `scan` treat `pattern` as a regex and enforce the
[regex guard limits](#guard-limits).

```vibe
"2024-03-05".match("([0-9]+)-([0-9]+)") # ["2024-03", "2024", "03"]
```

### Slicing and Concatenation

- `slice(index) -> string | nil` – single character at `index`; `nil` when out
  of bounds.
- `slice(index, length) -> string | nil` – substring of up to `length`
  characters starting at `index`.
- `concat(*strings) -> string` – receiver with all arguments appended.
- `prepend(*strings) -> string` – receiver with all arguments prepended, in
  order.
- `insert(index, string) -> string` – receiver with `string` inserted at a
  character index. A non-negative index inserts before the character at that
  position (the length appends); a negative index inserts after the character it
  selects (`-1` appends). An out-of-range index raises an error.
- `replace(replacement) -> string` – returns `replacement` (compatibility
  shim for Ruby's mutating `replace`).
- `clear -> string` – returns `""`.

### Case and Ordering Transforms

- `upcase(mode = nil) -> string` – uppercase all characters using full Unicode
  case mapping (locale-insensitive), so `"Straße".upcase` is `"STRASSE"`. Pass
  `:ascii` to map only ASCII letters.
- `downcase(mode = nil) -> string` – lowercase all characters using full Unicode
  case mapping. Pass `:ascii` for ASCII-only mapping or `:fold` for Unicode case
  folding (so `"Straße".downcase(:fold)` is `"strasse"`).
- `capitalize(mode = nil) -> string` – titlecase the first character and
  lowercase the rest. Pass `:ascii` to map only ASCII letters.
- `swapcase(mode = nil) -> string` – flip the case of each cased character,
  including cased non-letters such as circled letters and Roman numerals. Pass
  `:ascii` to map only ASCII letters.
- `reverse -> string` – characters in reverse order.

### Whitespace and Affix Trimming

- `strip -> string` – trim leading and trailing whitespace. Like Ruby, only the
  ASCII whitespace bytes `\0 \t \n \v \f \r " "` are removed, with `\0` trimmed
  from both ends; Unicode spaces such as NBSP (`U+00A0`), the Ogham space mark
  (`U+1680`), em space (`U+2003`), and the BOM (`U+FEFF`) are preserved.
- `lstrip -> string` – trim leading whitespace (same ASCII set as `strip`,
  including a leading `\0`).
- `rstrip -> string` – trim trailing whitespace (same ASCII set as `strip`,
  including a trailing `\0`).
- `squish -> string` – trim both ends and collapse internal whitespace runs to
  a single space. Unlike `strip`, `squish` also collapses Unicode whitespace.
- `chomp(separator = nil) -> string` – remove one trailing `"\r\n"`, `"\n"`,
  or `"\r"`; with a `separator` remove that suffix once; with `""` remove all
  trailing newlines.
- `chop -> string` – remove the last character; a trailing `"\r\n"` is removed
  as a single unit, otherwise one full Unicode character is removed; an empty
  string is returned unchanged.
- `delete_prefix(prefix) -> string` – remove `prefix` when present.
- `delete_suffix(suffix) -> string` – remove `suffix` when present.

### Padding

`width` counts Unicode characters (like `length`/`slice`); a `Float` width is
truncated toward zero. A width at or below the receiver's length returns it
unchanged. The pad string defaults to `" "`, must be non-empty, and is repeated
then truncated at a character boundary to fill the span.

- `center(width, pad = " ") -> string` – pad both sides, with the extra
  character on the right when the padding cannot be split evenly.
- `ljust(width, pad = " ") -> string` – left-justify, padding on the right.
- `rjust(width, pad = " ") -> string` – right-justify, padding on the left.

### Replacement, Splitting, and Templating

- `sub(pattern, replacement, regex: false) -> string` – replace the first
  occurrence of `pattern`. Given a block instead of `replacement`, the block
  receives the matched substring and its result replaces the match.
- `gsub(pattern, replacement, regex: false) -> string` – replace every
  occurrence of `pattern`. Given a block instead of `replacement`, the block
  receives each matched substring and its result replaces that match.
- `split(separator = nil) -> array` – split on runs of ASCII whitespace
  (space, tab, newline, vertical tab, form feed, carriage return; dropping empty
  fields) without arguments, or on `separator` when given. Like Ruby, the
  no-argument form keeps wider Unicode whitespace such as the non-breaking space
  inside the field rather than splitting on it.
- `chars -> array` – array of the string's Unicode characters, one per code
  point (rune-aware, like `length` and `slice`).
- `lines -> array` – array of lines split on `"\n"`, retaining the trailing
  newline on each line; an empty string yields no lines and carriage returns
  stay attached so `"\r\n"` endings round-trip.
- `bytes -> array` – array of the string's bytes as integers in `0..255`
  (byte-level, so a multibyte character expands to one entry per UTF-8 byte).
- `codepoints -> array` – array of the string's Unicode code points as integers
  (rune-aware, so a multibyte character is one entry; the integer counterpart to
  `chars`).
- `template(context, strict: false) -> string` – interpolate `{{key.path}}`
  placeholders from a hash; `strict: true` errors on missing placeholders.

With `regex: true`, `sub`/`gsub` compile `pattern` as a regex and expand
Ruby-style backreferences in `replacement`: `\1`–`\9` insert capture groups,
`\&` (or `\0`) the whole match, `` \` `` and `\'` the pre/post-match, `\+` the
last participating group, `\k<name>` a named group, and `\\` a literal
backslash. `$1` and `$&` are literal text, matching Ruby. See
[String#sub replacement backreferences](strings.md#replacement-backreferences)
for the full table. The regex [guard limits](#guard-limits) still apply.

The block forms of `sub`/`gsub` honor the same `regex` keyword (defaulting to
literal matching) and reject being given both a `replacement` argument and a
block. `scan(pattern)` and `match(pattern)` also accept blocks: `scan` yields
each match and returns the receiver, while `match` yields the match data and
returns the block's result (or `nil`, without invoking the block, when there is
no match). See [Strings](strings.md) for examples.

### Bang Variants

Each of the following returns the transformed string, or `nil` when the
transform changed nothing: `strip!`, `lstrip!`, `rstrip!`, `squish!`,
`chomp!`, `chop!`, `delete!`, `delete_prefix!`, `delete_suffix!`, `tr!`,
`squeeze!`, `upcase!`, `downcase!`, `capitalize!`, `swapcase!`, `reverse!`.
`sub!` and `gsub!` key their result off the match instead: they return the
rewritten string whenever the pattern matched (even when the replacement
reproduces the original text) and `nil` only when it never matched.

## Arrays

See [arrays.md](arrays.md) for worked examples. Arrays also support `+`
(concatenation) and `-` (value subtraction) operators.

### Inspection

- `size -> int` – element count.
- `length -> int` – alias for `size`.
- `empty? -> bool` – true when the array has no elements.
- `inspect -> string` – debug rendering with each element inspected
  recursively (see [Debug Representation](#debug-representation)).

### Iteration

- `each { |item| } -> array` – yield each element; returns the receiver.
- `each_with_index { |item, index| } -> array` – yield each element with its
  0-based index; returns the receiver. Takes no arguments.
- `each_slice(n) { |slice| } -> nil` – yield non-overlapping slices of length
  `n` (the trailing slice may be shorter); `n` must be a positive integer.
- `each_cons(n) { |window| } -> nil` – yield each sliding window of length `n`;
  arrays shorter than `n` yield nothing and `n` must be a positive integer.
- `reverse_each { |item| } -> array` – yield elements from last to first;
  returns the receiver.
- `cycle(n = nil) { |item| } -> nil` – yield the whole array `n` times; a
  non-positive `n` yields nothing. Omitting `n` or passing `nil` cycles forever,
  bounded by the step quota and context cancellation.
- `map { |item| } -> array` – new array of block results.
- `map_with_index { |item, index| } -> array` – new array of block results,
  passing each element's 0-based index to the block. Takes no arguments.
- `filter_map { |item| } -> array` – block results that are truthy; fuses `map`
  with a truthiness filter, dropping falsy returns.
- `select { |item| } -> array` – elements for which the block is truthy.
- `reject { |item| } -> array` – elements for which the block is falsy (the
  inverse of `select`).
- `take_while { |item| } -> array` – leading elements until the block first
  returns a falsy value; stops at the first miss.
- `drop_while { |item| } -> array` – elements remaining after skipping the
  leading run for which the block is truthy.
- `grep(pattern) { |item| } -> array` – elements that match `pattern` using the
  case-equality direction (`pattern === item`); a `Range` matches by membership
  and other values by equality. The optional block transforms each match.
- `grep_v(pattern) { |item| } -> array` – elements that do not match `pattern`,
  with the same matching rules and optional transform block as `grep`.
- `find { |item| } -> value | nil` – first element matching the block, or
  `nil` when none does; write the miss handling after the call.
- `find_index(value) -> int | nil` / `find_index { |item| } -> int | nil` –
  index of the first element equal to `value`, or the first index whose block is
  truthy. Alias for `index`; pass a value or a block, never both.
- `reduce(initial = nil) { |acc, item| } -> value` – fold left; without
  `initial` the first element seeds the accumulator. An empty array folds to
  `nil` when no `initial` is given, or to `initial` when one is.
- `reduce(operation) -> value` and `reduce(initial, operation) -> value` –
  fold by sending `operation` to the accumulator with each element, like Ruby's
  `["a", "b"].reduce(:concat)`. `operation` is a symbol naming a method on the
  accumulator (`["a", "b"].reduce(:concat)`) or a string naming either a method
  or a binary operator (`[1, 2, 3].reduce(:+)`, also `-`, `*`, `/`, `%`, `**`).
  With a block and a single argument, the block takes precedence and the lone
  argument is treated as `initial`. With two arguments (`reduce(initial,
  operation)`) the operation is always used and any block is ignored, matching
  Ruby (`[1, 2, 3].reduce(10, :+) { |a, b| a * b }` folds with `+`).

### Membership and Counting

- `include?(value) -> bool` – membership test using value equality.
- `index(value, offset = 0) -> int | nil` / `index { |item| } -> int | nil` –
  first index of `value` at or after `offset`, or the first index whose block is
  truthy. Pass a value or a block, never both.
- `rindex(value, offset = last_index) -> int | nil` /
  `rindex { |item| } -> int | nil` – last index of `value` at or before
  `offset`, or the last index whose block is truthy. Pass a value or a block,
  never both.
- `fetch(index, default) -> value` / `fetch(index) { |index| } -> value` –
  element at `index` (negative counts from the end). When `index` is out of
  bounds, evaluates the block with the requested index if a block is given,
  otherwise returns the `default` argument if given, otherwise raises `index
  ... outside of array bounds`. When both a `default` and a block are supplied,
  the block supersedes the default and is evaluated on a miss, matching Ruby.
- `dig(*path) -> value | nil` – nested lookup following `path`. Each component
  descends one level: an integer index into an array or a symbol/string key
  into a hash, so a single `dig` can traverse mixed array/hash data. `nil` when
  any step is missing or out of range; a non-integer array index raises.
- `count -> int` – element count.
- `count(value) -> int` – occurrences of `value`.
- `count { |item| } -> int` – elements for which the block is truthy.
- `any? { |item| } -> bool` – true when any element (or block result) is
  truthy.
- `all? { |item| } -> bool` – true when every element (or block result) is
  truthy.
- `none? { |item| } -> bool` – true when no element (or block result) is
  truthy.
- `any?(pattern)`, `all?(pattern)`, `none?(pattern) -> bool` – test each element
  against `pattern` with case equality (`===`), so range patterns test
  membership (`[2].any?(1..3)` is true). A `pattern` argument takes precedence
  over an attached block.
- `one? { |item| } -> bool` – true when exactly one element (or block result)
  is truthy.

### Building and Slicing

- `push(*values) -> array` – appends `values` to the receiver in place and
  returns the receiver. Accepts zero values: bare `push` and `push()` are
  no-ops returning the receiver, matching Ruby.
- `append(*values) -> array` – Ruby-style alias for `push`.
- `prepend(*values) -> array` – inserts `values` at the front of the receiver
  in place and returns it, so `[3].prepend(1, 2)` is `[1, 2, 3]`.
- `unshift(*values) -> array` – Ruby-style alias for `prepend`.
- `pop -> value | nil` / `pop(n) -> array` – removes element(s) from the end of
  the receiver in place; bare `pop` returns the removed element (`nil` on an
  empty array), `pop(n)` removes up to `n` and returns them as an array.
- `shift -> value | nil` / `shift(n) -> array` – removes element(s) from the
  front of the receiver in place, mirroring `pop`. `n` must be a non-negative
  integer.
- `delete(value) -> value | nil` / `delete(value) { default } -> value` –
  removes every element equal to `value` from the receiver in place, returning
  the last removed element, or `nil` on a miss (the block result instead when a
  block is given).
- `insert(index, *values) -> array` – splices `values` into the receiver before
  the element at `index` and returns the receiver. A negative index inserts
  after that element (`insert(-1, x)` appends); an index past the end pads with
  `nil`; a negative index past the start raises. Inserting no values returns
  the receiver unchanged.
- `clear -> array` – removes every element from the receiver and returns it.
- `delete_if { |item| } -> array` / `keep_if { |item| } -> array` – prune the
  receiver against the block (drop accepted / keep accepted) and return it.
- `first -> value | nil` / `first(n) -> array` – leading element(s).
- `last -> value | nil` / `last(n) -> array` – trailing element(s).
- `uniq -> array` – distinct values, keeping first occurrences.
- `compact -> array` – elements with `nil` entries removed.
- `flatten(depth = nil) -> array` – collapse nested arrays. No argument, `nil`,
  or a negative depth flattens fully; `0` returns a shallow copy; a positive
  depth flattens that many levels and a `Float` depth is truncated to an integer.
  A nonnumeric depth raises.
- `chunk(size) -> array` – consecutive slices of `size` elements (last chunk
  may be shorter).
- `window(size) -> array` – overlapping windows of `size` elements; empty when
  `size` exceeds the array length.
- `join(separator = "") -> string` – stringified elements joined by
  `separator`.
- `reverse -> array` – elements in reverse order.
- `to_h -> hash` / `to_h { |element| [key, value] } -> hash` – build a hash from
  two-element `[key, value]` pairs (the inverse of `Hash#to_a`). Keys use the
  same Ruby-style hash-key identity as hash literals and duplicate keys keep the
  last pair; the block form maps each element to its pair. A non-array element,
  a pair that is not exactly two elements, or an unsupported key raises.

The removal helpers mutate the receiver and hand back what they removed,
exactly as in Ruby:

```vibe
items = [1, 2, 3, 4, 5]
items.pop       # 5    (items is now [1, 2, 3, 4])
items.pop(2)    # [3, 4]
items.shift     # 1
items.shift(2)  # [2] (clamped to the remaining length)
[1, 2, 2].delete(2) # 2
```

### Aggregation, Ordering, and Grouping

- `sum -> int | float` – total of numeric elements (`0` for an empty array).
- `sort -> array` – stable sort using natural ordering.
- `sort { |a, b| } -> array` – stable sort using a comparator block returning
  a negative, zero, or positive number.
- `sort_by { |item| } -> array` – stable sort by the block's key for each
  element.
- `partition { |item| } -> array` – `[matching, non_matching]` pair of arrays.
- `group_by { |item| } -> hash` – group elements by block result.
- `group_by_stable { |item| } -> array` – `[key, items]` pairs preserving
  first-seen group order.
- `tally -> hash` / `tally { |item| } -> hash` – occurrence counts keyed by
  element (or block result).
- `min -> value | nil` / `max -> value | nil` – smallest/largest element using
  natural ordering; `nil` for an empty array.
- `minmax -> array` – `[min, max]` in one pass; `[nil, nil]` for an empty array.
- `min_by { |item| } -> value | nil` / `max_by { |item| } -> value | nil` –
  element with the smallest/largest block key; `nil` for an empty array. Ties
  resolve to the first matching element.

String and symbol ordering uses deterministic codepoint comparison (no locale
collation).

```vibe
[5, 1, 4].sort do |a, b|
  b - a
end
# [5, 4, 1]
```

## Hashes

See [hashes.md](hashes.md) for worked examples. Hash keys live in one string
keyspace: symbols and strings address the same entry, and integer or array
keys are rejected. `keys`, `values`, and all block-based iteration visit
entries in Ruby-style insertion order for determinism.

Property access (`record.name`) resolves the hash methods below before stored
keys, so method names stay stable even when data contains the same key:

```vibe
sizes = { size: "XL" }
sizes.size            # 1
sizes[:size]          # "XL"
{ color: "red" }.size # 1
```

Use index access (`hash[:size]`) to read entries whose names collide with hash
methods.

### Inspection

- `size -> int` – entry count.
- `length -> int` – alias for `size`.
- `empty? -> bool` – true when the hash has no entries.
- `key?(key) -> bool` – true when `key` is present.
- `has_key?(key) -> bool` – alias for `key?`.
- `member?(key) -> bool` – alias for `key?`.
- `include?(key) -> bool` – alias for `key?`.
- `value?(value) -> bool` – true when any stored value equals `value` using `==`.
- `has_value?(value) -> bool` – alias for `value?`.
- `inspect -> string` – debug rendering using colon-label keys with each value
  inspected recursively (see [Debug Representation](#debug-representation)).

### Access

- `fetch(key, default) -> value` / `fetch(key) { |key| } -> value` – value for
  `key`. When `key` is missing, evaluates the block with the requested key if a
  block is given, otherwise returns the `default` argument if given, otherwise
  raises `key not found`. When both a `default` and a block are supplied, the
  block supersedes the default and is evaluated on a miss, matching Ruby. Use
  `[]` or `dig` when a missing key should yield `nil`.
- `fetch_values(*keys) { |key| } -> array` – values for `keys` in requested
  order. Raises `key not found` for any missing key; when a block is given it is
  called with each missing key and its result is used instead.
- `dig(*path) -> value | nil` – nested lookup following `path`. Each component
  descends one level: a symbol/string key into a hash or an integer index into
  an array, so a single `dig` can traverse mixed hash/array data. `nil` when any
  step is missing or out of range; a non-integer array index raises.
- `keys -> array` – keys in insertion order.
- `values -> array` – values in insertion order.

### Iteration

- `each { |key, value| } -> hash` – yield each pair; returns the receiver.
- `each_with_index { |pair, index| } -> hash` – yield each `[key, value]` pair
  with its 0-based index in insertion order, matching Ruby's
  `Hash#each_with_index`; returns the receiver. Takes no arguments.
- `each_key { |key| } -> hash` – yield each key.
- `each_value { |value| } -> hash` – yield each value.
- `to_a -> array` – nested `[key, value]` pairs in insertion order, with keys
  exposed as strings. The inverse of `Array#to_h`, equivalent to `flatten(0)`.
- `map_with_index { |pair, index| } -> array` – new array of block results,
  yielding each `[key, value]` pair with its 0-based index in insertion order.
  Takes no arguments.

### Transform and Filter

- `merge(*others) -> hash` – combined entries from the receiver and every
  argument hash. Arguments are applied left to right, so later hashes win on key
  conflicts. With no arguments (including the bare, parenless `hash.merge`) it
  returns a copy of the receiver.
- `merge(*others) { |key, old_value, new_value| } -> hash` – combined entries; for
  keys present in both hashes the block resolves the conflict and its result is
  stored, folding through each argument in turn. Keys present on only one side are
  copied without invoking the block, and the conflict key is the stored string.
- `replace(other) -> hash` – discards the receiver's entries and adopts
  `other`'s, updating the receiver the call names and returning it. Hashes
  have no per-hash default metadata.
- `flatten(depth = 1) -> array` – flat array built from the `[key, value]` pairs,
  flattened to `depth`. The default depth produces `[key, value, ...]`; array
  values stay nested unless a deeper `depth` is given. A `depth` of `0` keeps the
  pairs nested, a negative `depth` flattens completely, and a `Float` depth is
  truncated. Entries are emitted in insertion order.
- `store(key, value) -> value` – Ruby's method spelling of index assignment:
  writes the entry into the receiver in place and returns the stored value. An
  existing key keeps its position in the insertion order.
- `delete(key) -> value | nil` / `delete(key) { |key| default } -> value` –
  removes the entry from the receiver in place and returns the removed value.
  On a miss it returns `nil` — or the block result for the requested key — and
  leaves the receiver untouched.
- `clear -> hash` – empties the receiver in place and returns it.
- `delete_if { |key, value| } -> hash` / `keep_if { |key, value| } -> hash` –
  prune the receiver in place against the block (drop accepted / keep accepted
  entries) and return it; survivors keep their insertion order.
- `slice(*keys) -> hash` – only the listed keys; missing keys are skipped.
  A candidate that is not a string or symbol raises.
- `except(*keys) -> hash` – all entries except the listed keys. A candidate
  that is not a string or symbol raises.
- `select { |key, value| } -> hash` – entries for which the block is truthy.
- `reject { |key, value| } -> hash` – entries for which the block is falsy.
- `compact -> hash` – entries with `nil` values removed.
- `transform_keys { |key| } -> hash` – rename keys via the block (must return
  a symbol or string).
- `deep_transform_keys { |key| } -> hash` – `transform_keys` applied
  recursively through nested hashes and arrays; rejects cyclic structures.
- `remap_keys(mapping) -> hash` – rename keys using a `{ old: :new }` hash;
  unmapped keys pass through.
- `transform_values { |value| } -> hash` – replace each value with the block
  result.

## Integers

Integers are arbitrary precision: arithmetic, literals, comparisons,
string conversion, and JSON promote transparently past the signed
64-bit range, and results that fit again return to the compact 64-bit
representation. Integers are not hash keys — convert with `to_s` first.
Two values of the same integer are always `==` and `eql?`;
`equal?` follows Ruby's object model, where small (64-bit) integers are
value-identical but two separately produced big integers are distinct
objects. The deliberate 64-bit boundaries — each raising a clear error for a
larger value — are range endpoints, the iteration members (`times`, `upto`,
`downto`, `step`), `Money`/`Duration`/`Time` arithmetic, and argument
positions denoting indexes, counts, sizes, or precisions. Integer literals
parse up to 100,000 digits (a parser cost guard; larger values remain
constructible through arithmetic).

### Duration Constructors

Each returns a `duration` spanning that many units. Singular forms are
aliases, so `1.second` reads naturally.

- `seconds` / `second` -> duration
- `minutes` / `minute` -> duration
- `hours` / `hour` -> duration
- `days` / `day` -> duration
- `weeks` / `week` -> duration

### Numeric Helpers

- `abs -> int` – absolute value; the minimum 64-bit integer promotes to
  `9223372036854775808` rather than erroring.
- `clamp(min, max) -> int | float` / `clamp(range) -> int` – receiver bounded
  to the given bounds; integer and float bounds may be mixed, `nil` leaves one
  side open, and range form accepts inclusive integer ranges.
- `even? -> bool` – true for even integers.
- `odd? -> bool` – true for odd integers.
- `times { |i| } -> int` – run the block with `0..n-1`; returns the receiver.
- `upto(limit) { |i| } -> int` – run the block with each integer from the
  receiver up to `limit` inclusive (nothing when the receiver already exceeds
  `limit`); returns the receiver.
- `downto(limit) { |i| } -> int` – run the block with each integer from the
  receiver down to `limit` inclusive (nothing when the receiver is already below
  `limit`); returns the receiver.
- `step(limit, by = 1) { |i| } -> int` – run the block with the receiver and
  each subsequent value `by` apart, while it has not passed `limit` (`<= limit`
  for a positive step, `>= limit` for a negative step); `by` must be a nonzero
  integer. Returns the receiver.
- `zero? -> bool` – true when the integer is `0`.
- `positive? -> bool` – true when greater than `0`.
- `negative? -> bool` – true when less than `0`.
- `nonzero? -> int?` – the receiver when nonzero, otherwise `nil`, matching
  Ruby (the result is truthy exactly when the number is nonzero).
- `next -> int` / `succ -> int` – the next integer (`self + 1`), promoting
  past the 64-bit boundary.
- `pred -> int` – the previous integer (`self - 1`), promoting past the
  64-bit boundary.
- `round(ndigits = 0) -> int` – non-negative `ndigits` return the receiver
  unchanged; negative `ndigits` round to the matching power of ten (e.g.
  `1234.round(-2)` is `1200`) half away from zero.
- `floor(ndigits = 0) -> int` – like `round`, but negative `ndigits` truncate
  toward negative infinity (`1234.floor(-2)` is `1200`, `(-1234).floor(-2)` is
  `-1300`).
- `ceil(ndigits = 0) -> int` – like `round`, but negative `ndigits` round
  toward positive infinity (`1234.ceil(-2)` is `1300`).
- `div(n) -> int` – floored division; the quotient rounds toward negative
  infinity, so mixed-sign operands round down (`(-5).div(2)` is `-3`). A zero
  divisor errors; quotients outside the 64-bit range promote.
- `divmod(n) -> [quotient, modulo]` – the floored quotient and the modulo whose
  sign follows the divisor. With integer arguments both elements are integers;
  a float argument makes the modulo a float.
- `fdiv(n) -> float` – floating division. As in Ruby, a zero divisor follows
  IEEE 754: a finite nonzero receiver yields `Infinity`/`-Infinity` and a zero
  receiver yields `NaN`, matching the `/` operator (integer `/` still errors).
- `remainder(n) -> int|float` – remainder whose sign follows the receiver
  (truncated division), which differs from `%` for operands of opposite sign;
  a zero divisor errors.
- `modulo(n) -> int|float` – the `%` operator as a method: the result's sign
  follows the divisor (floored division). Integer operands yield an integer;
  any float operand yields a float; a zero divisor errors.
- `to_s -> string` – the integer's display digits (Ruby's `Integer#to_s`).
- `string -> string` – alias for `to_s`.
- `to_i -> int` – the receiver itself.
- `to_f -> float` – the value as a float.
- `nil? -> bool` – always `false`.
- `inspect -> string` – the integer's debug rendering (same digits as `to_s`;
  see [Debug Representation](#debug-representation)).

`round`, `floor`, and `ceil` accept an optional Integer precision. As in Ruby,
the precision must fit a 32-bit signed integer (Ruby reads it through `NUM2INT`),
so a magnitude beyond that range raises rather than acting as a no-op. Results
that leave the 64-bit integer range widen to arbitrary precision exactly like
Ruby's; a bucket sized by an astronomical precision (for example
`1.ceil(-1000000000)`, whose result is `10 ** 1000000000`) is preflighted
against the sandbox's memory quota rather than materialized blindly.

## Floats

- `abs -> float` – absolute value.
- `clamp(min, max) -> int | float` / `clamp(range) -> float` – receiver
  bounded to the given bounds; integer and float bounds may be mixed, `nil`
  leaves one side open, and range form accepts inclusive integer ranges.
- `round(ndigits = 0) -> int | float` – round half away from zero. With no
  argument or `0` it returns an `int`; positive `ndigits` keep the value a
  `float` rounded to that many fractional digits (`1.234.round(2)` is `1.23`);
  negative `ndigits` return an `int` bucketed to a power of ten.
- `floor(ndigits = 0) -> int | float` – round toward negative infinity, with
  the same `int`/`float` return rules as `round`.
- `ceil(ndigits = 0) -> int | float` – round toward positive infinity, with the
  same `int`/`float` return rules as `round`.
- `zero? -> bool` – true when the value is `0.0`.
- `positive? -> bool` – true when greater than `0.0`.
- `negative? -> bool` – true when less than `0.0`.
- `nonzero? -> float?` – the receiver when nonzero, otherwise `nil`, matching
  Ruby (the result is truthy exactly when the number is nonzero).
- `nan? -> bool` – true when the value is the IEEE `NaN` (for example
  `(0.0 / 0.0).nan?`).
- `infinite? -> int?` – `1` for `Infinity`, `-1` for `-Infinity`, and `nil` for
  every finite value and `NaN`, matching Ruby. The result is truthy exactly when
  the value is infinite.
- `finite? -> bool` – true when the value is neither infinite nor `NaN`.
- `div(n) -> int` – floored division returning an integer; a zero divisor
  errors; quotients outside the 64-bit range promote.
- `divmod(n) -> [int, float]` – the floored quotient (an integer) and the
  float modulo whose sign follows the divisor. A zero divisor errors.
- `fdiv(n) -> float` – floating division. As in Ruby, a zero divisor follows
  IEEE 754: a finite nonzero receiver yields `Infinity`/`-Infinity` and a zero
  receiver yields `NaN`, matching the `/` operator.
- `remainder(n) -> float` – remainder whose sign follows the receiver
  (truncated division); a zero divisor errors.
- `modulo(n) -> float` – the `%` operator as a method: the result's sign
  follows the divisor (floored division); a zero divisor errors.
- `to_s -> string` – the float's display text (Ruby's `Float#to_s`).
- `string -> string` – alias for `to_s`.
- `to_i -> int` – truncate toward zero (Ruby's `Float#to_i`); finite values
  beyond 64 bits promote to exact integers, and a non-finite value raises
  rather than producing garbage.
- `to_f -> float` – the receiver itself.
- `nil? -> bool` – always `false`.
- `inspect -> string` – the float's debug rendering (same text as `to_s`,
  including `Infinity`/`-Infinity`/`NaN`; see
  [Debug Representation](#debug-representation)).

Float division by zero with the `/` operator follows IEEE 754 like Ruby:
`1.0 / 0` is `Infinity`, `-1.0 / 0` is `-Infinity`, and `0.0 / 0.0` is `NaN`.
Integer division by zero (`1 / 0`) still raises. Special values print as
`Infinity`, `-Infinity`, and `NaN`, and `div`, `divmod`, `modulo`, and
`remainder` keep raising on a zero divisor (they return floored or
integer-valued results, for which Ruby also raises). `JSON.stringify` rejects
non-finite floats because JSON has no representation for them.

Comparisons follow IEEE 754 and Ruby. Infinities order as the extreme values
(`Infinity > 1000000.0`). Any comparison involving `NaN` is unordered: `<`,
`<=`, `>`, and `>=` all return `false`, equality is `false` (so `NaN == NaN` is
`false`), and the spaceship operator `<=>` returns `nil`. Coercing a non-finite
float to an integer raises rather than silently producing a garbage value, so
a `NaN` or `Infinity` endpoint in a range, a non-finite `money_cents` amount, or
non-finite duration arithmetic reports a clear error.

`round`, `floor`, and `ceil` accept an optional Integer precision that defaults
to `0`. As in Ruby, the precision must fit a 32-bit signed integer, so a
magnitude beyond that range raises rather than acting as a no-op. Whenever the
result is converted back to an `int` (zero or negative precision), finite
values outside the 64-bit integer range promote to exact integers, matching
Ruby; only `NaN` and the infinities raise.

Vibescript has no rational number type, so Ruby's `quo` (which returns a
`Rational` for integer operands) is intentionally not provided; use `fdiv` for
floating division.

## Money

Money values are created with the `money` and `money_cents` builtins and
support arithmetic and comparison operators.

- `currency -> string` – ISO currency code, e.g. `"USD"`.
- `cents -> int` – total amount in minor units.
- `amount -> string` – formatted amount with currency, e.g. `"100.50 USD"`.
- `format -> string` – same as `amount`.
- `to_s` / `string` -> string – same as `amount`.

```vibe
m = money("100.50 USD")
m.cents    # 10050
m.currency # "USD"
m.amount   # "100.50 USD"
```

## Durations

See [durations.md](durations.md) for arithmetic and worked examples.

### Whole-Unit Conversions

Each returns an `int` truncated toward zero. Singular forms are aliases.

- `seconds` / `second` -> int – total seconds.
- `minutes` / `minute` -> int – total whole minutes.
- `hours` / `hour` -> int – total whole hours.
- `days` / `day` -> int – total whole days.
- `weeks` / `week` -> int – total whole weeks.

### Fractional Conversions

Each returns a `float`. Months use 30-day and years 365-day approximations.

- `in_seconds -> float`
- `in_minutes -> float`
- `in_hours -> float`
- `in_days -> float`
- `in_weeks -> float`
- `in_months -> float` – approximate (30-day months).
- `in_years -> float` – approximate (365-day years).

```vibe
90.seconds.minutes    # 1 (truncated)
90.seconds.in_minutes # 1.5
```

### Formatting and Conversion

- `iso8601 -> string` – ISO 8601 duration, e.g. `"PT1H30M"`.
- `parts -> hash` – `{ days:, hours:, minutes:, seconds: }` breakdown.
- `to_i -> int` – total seconds.
- `to_s -> string` – seconds string, e.g. `"5400s"`.
- `string -> string` – alias for `to_s`.
- `format -> string` – same as `to_s`.
- `eql?(other) -> bool` – true when both durations span the same seconds.

```vibe
shift = 90.minutes
shift.parts   # {days: 0, hours: 1, minutes: 30, seconds: 0}
shift.iso8601 # "PT1H30M"
```

### Anchoring to Times

Each accepts an optional `Time` (or RFC3339 string) and defaults to the
current time; the result is a UTC `Time`.

- `after(start = Time.now) -> time` – `start` plus the duration.
- `since(start = Time.now) -> time` – alias for `after`.
- `from_now(start = Time.now) -> time` – alias for `after`.
- `ago(start = Time.now) -> time` – `start` minus the duration.
- `before(start = Time.now) -> time` – alias for `ago`.
- `until(start = Time.now) -> time` – alias for `ago`.

```vibe
5.minutes.ago(Time.utc(2024, 1, 1)).iso8601 # "2023-12-31T23:55:00Z"
```

## Times

See [time.md](time.md) for construction, zone handling, and layout-based
formatting. Times also support `time + duration`, `time - duration`,
`time + number` / `time - number` (the number is seconds, matching Ruby), and
`time - time -> float` (seconds) arithmetic.

### Components

- `year -> int` – calendar year.
- `month` / `mon` -> int – month of year (1-12).
- `day` / `mday` -> int – day of month.
- `hour -> int` – hour of day (0-23).
- `min -> int` – minute of hour.
- `sec -> int` – second of minute.
- `usec` / `tv_usec` -> int – microsecond component.
- `nsec` / `tv_nsec` -> int – nanosecond component.
- `subsec -> float` – fractional second as a float.
- `wday -> int` – day of week (0 = Sunday).
- `yday -> int` – day of year (1-366).

### Zone and Offset

- `zone -> string` – zone abbreviation, e.g. `"UTC"`.
- `utc_offset` / `gmt_offset` / `gmtoff` -> int – offset from UTC in seconds.

### Predicates

- `utc?` / `gmt?` -> bool – true for UTC times.
- `dst?` / `isdst` -> bool – true when daylight saving time is in effect.
- `sunday?`, `monday?`, `tuesday?`, `wednesday?`, `thursday?`, `friday?`,
  `saturday?` -> bool – day-of-week checks.

### Conversions

- `to_i` / `tv_sec` -> int – seconds since the Unix epoch.
- `to_f -> float` – epoch seconds with fractional part.
- `to_r -> float` – same as `to_f` (rationals are not supported).
- `to_s -> string` – RFC3339Nano representation.
- `string -> string` – alias for `to_s`.
- `to_a -> array` – positional tuple `[sec, min, hour, mday, month, year, wday,
  yday, isdst, zone]`, matching Ruby's field order and the receiver's zone.
- `iso8601(ndigits = 0)` / `xmlschema(ndigits = 0)` / `rfc3339(ndigits = 0)` -> string – RFC3339 representation. With no argument it emits whole seconds; a non-negative `ndigits` appends that many fractional-second digits, truncated toward zero (matching Ruby's `Time#iso8601`). `xmlschema` is an alias for `iso8601`. Negative, non-integer, or out-of-range (above 100 digits) precision raises a runtime error.
- `httpdate -> string` – HTTP-date / IMF-fixdate form (RFC 7231), always rendered in GMT, e.g. `"Tue, 02 Jan 2024 03:04:05 GMT"`. Takes no arguments.
- `rfc2822 -> string` / `rfc822 -> string` – RFC 2822 mail date preserving the receiver's zone offset, e.g. `"Tue, 02 Jan 2024 03:04:05 -0000"`. A genuine UTC receiver uses the `-0000` zone Ruby reserves for timestamps without real zone information; an explicit zero offset uses `+0000`. Both drop sub-second precision and take no arguments.
- `hash -> int` – nanoseconds since the Unix epoch (identity value).

### Zone Conversion

- `utc` / `gmtime` -> time – the same instant in UTC.
- `getutc` / `getgm` -> time – aliases for `utc`.
- `localtime(offset = nil) -> time` – the same instant in the supplied zone,
  or the host's local zone when the argument is omitted or `nil`. The offset
  follows the usual zone rules: a fixed offset such as `"+05:30"` or `"-04:00"`,
  a named zone such as `"America/New_York"`, or `"UTC"`. Returns a new `Time`;
  the receiver is never mutated.
- `getlocal(offset = nil) -> time` – alias for `localtime`.

### Formatting

- `format(layout) -> string` – format with a Go layout string (reference time
  `Mon Jan 2 15:04:05 MST 2006`).
- `strftime(format) -> string` – format with a Ruby-style percent format string
  (e.g. `"%Y-%m-%d %H:%M:%S"`). Supports the common directive subset; unknown
  directives pass through verbatim, while a trailing `%` with no directive raises
  a runtime error. See [time.md](time.md) for the directive table.

### Comparison and Rounding

- `<=>(other) -> int | nil` – `-1`, `0`, or `1` ordering against another time,
  or `nil` when `other` is not a time (matching Ruby's spaceship contract).
- `eql?(other) -> bool` – true when both times are the same instant.
- `round(ndigits = 0) -> time` – round to the given number of fractional-second
  digits, half away from zero. No argument or `0` rounds to whole seconds;
  positive `ndigits` rounds to that many digits (e.g. `3` for milliseconds, `6`
  for microseconds), capped at nanosecond resolution. `ndigits` must be a
  non-negative `Integer`; other values raise an error.
- `floor -> time` – truncate to the whole second.
- `ceil -> time` – round up to the next whole second.

## Symbols

Symbols (`:name`) expose the Ruby string/symbol conversion helpers:

- `id2name -> string` – the symbol's name as a string.
- `to_s -> string` – alias for `id2name`.
- `string -> string` – alias for `to_s`.
- `to_sym -> symbol` – returns the receiver unchanged.

`"name".to_sym` and `:name.to_s` round-trip between the two representations.
`:name == "name"` is still `false`; hash lookup is the exception, where both
address the same entry.

## Enum Values

Enum members obtained via `EnumName::member` expose three properties (see
[enums.md](enums.md)):

- `name -> string` – member name, e.g. `"active"`.
- `symbol -> symbol` – member symbol, e.g. `:active`.
- `enum -> enum` – the defining enum.

## Ranges

Ranges (`1..5`, `1...5`) are consumed by `for ... in` loops, and `case`/`when`
uses range candidates as numeric membership tests (see
[control-flow.md](control-flow.md)). They also expose query and conversion
helpers.

Vibescript iterates descending ranges such as `5..1` (yielding `5, 4, 3, 2, 1`),
so `size`, `to_a`, `first(n)`, and `last(n)` report that descending sequence
rather than the empty result Ruby produces. The other helpers match Ruby's
`Range` semantics.

### Membership

- `cover?(value)` / `include?(value)` / `member?(value)` -> bool – true when
  `value` falls within the range, honoring exclusive ends and range direction.
  Integer and float arguments are tested numerically; any other type is never a
  member and returns `false` rather than raising.

### Metadata

- `first -> int` – the start endpoint.
- `last -> int` – the end endpoint, ignoring exclusivity (matching Ruby).
- `size -> int` – the number of integers the range iterates over.
- `exclude_end? -> bool` – true for `...` ranges, false for `..` ranges.

### Conversion

- `first(n) -> array` – the first `n` iterated elements, clamped to the range.
- `last(n) -> array` – the last `n` iterated elements, clamped to the range.
  A negative `n` raises; a non-integer `n` raises.
- `to_a -> array` – every element the range iterates over, bounded by the
  sandbox step and memory quotas so large ranges fail safely instead of
  exhausting memory.

### Iteration

Each helper yields the range's integers in iteration order (ascending for
`1..5`, descending for `5..1`), charging one sandbox step per element so a wide
range fails on the step quota rather than running unbounded. Helpers that build
an array (`map`, `select`, `reject`) also honor the memory quota as the result
grows.

- `each { |i| } -> range` – run the block with each integer; returns the range.
- `step(n) { |i| } -> range` – run the block with every `n`-th integer starting
  at the range's start; `n` must be a positive integer. Iteration advances by the
  stride directly, so a sparse step over a wide span only charges the step quota
  for the values it yields. Returns the range.
- `map { |i| } -> array` – collect the block's result for each integer.
- `select { |i| } -> array` / `reject { |i| } -> array` – keep the integers for
  which the block is truthy (`select`) or falsy (`reject`).
- `find { |i| } -> int?` – the first integer for which the block is truthy, or
  `nil` when none match; short-circuits on the first match.
- `reduce(initial = first) { |acc, i| } -> value` – fold the integers with the
  block. With no argument the first integer seeds the accumulator; an empty
  range returns the seed, or `nil` when no seed is given.
- `count -> int` / `count { |i| } -> int` – the number of integers in the
  range, or, with a block, how many the block keeps.
- `sum(initial = 0) -> int` – the sum of the range's integers plus `initial`;
  totals past the 64-bit boundary promote to arbitrary precision.
- `min -> int?` / `max -> int?` – the smallest or largest integer the range
  iterates over, or `nil` for an empty range.

## Builtin Functions

Global functions and namespaces available in every script. See
[builtins.md](builtins.md) for narrative examples.

### Global Functions

- `assert(condition, message = nil, message: nil) -> nil` – raise an assertion
  failure when `condition` is falsy; the message comes from the second
  positional argument or the `message:` keyword.
- `money(literal) -> money` – parse a `"amount CURRENCY"` string, e.g.
  `money("25.00 USD")`.
- `money_cents(cents, currency) -> money` – build money from integer minor
  units, e.g. `money_cents(2550, "USD")`.
- `now -> string` – current UTC instant as an RFC3339 string (use `Time.now`
  for a `time` value).
- `loop { ... } -> value` – repeat the block until `break`; a `break value`
  becomes the result and `next` starts the next iteration.
- `format(pattern, *values) -> string` / `sprintf(pattern, *values) -> string`
  – format common numeric and string values with percent format strings. Output
  is capped at 1 MiB before width or precision padding is materialized.
- `rand(max = nil) -> number` – random float in `[0.0, 1.0)`, integer below a
  positive integer bound, or integer inside an integer range.
- `srand(seed = nil) -> int | nil` – seed this script call's `rand` sequence;
  returns the previous explicit seed when one exists.
- `uuid -> string` – RFC 9562 version 7 UUID.
- `random_id(length = 16) -> string` – unbiased alphanumeric token; `length`
  must be between 1 and 1024.
- `to_int(value) -> int` – convert an int, integral float, or base-10 numeric
  string; errors otherwise.
- `to_float(value) -> float` – convert an int, float, or finite numeric
  string; errors otherwise.
- `require(module_name, as: nil) -> object` – load a module and return its
  namespace; `as:` binds that namespace to a name. Exported functions are
  direct call targets, not detachable values. See
  [builtins.md](builtins.md#module-loading).

`now` and `uuid` auto-invoke, so they can be called without parentheses.

### JSON

- `JSON.parse(string) -> value` – parse JSON into hashes, arrays, strings,
  ints, floats, bools, and nils; rejects trailing data. Integer tokens beyond
  64 bits parse as exact integers (they previously degraded to floats);
  float-looking tokens stay floats.
- `JSON.stringify(value) -> string` – serialize hashes/objects, arrays, and
  scalars; symbols and enum values become strings; rejects cyclic structures.
  Integers of any size emit their full decimal form (no float collapse, no
  quotes), matching Ruby's JSON.

Both directions enforce a 1 MiB payload limit and reject more than 10,000
nested arrays/objects.

### Math

The `Math` namespace mirrors Ruby's `Math` module. Constants read with either
accessor (`Math::PI` or `Math.PI`); helpers are called like `Math.sqrt(9)`.
Integer arguments are promoted to floats and every helper returns a `float`.

Constants:

- `Math::PI -> float` – circle constant.
- `Math::E -> float` – natural-logarithm base.

Functions:

- `Math.sqrt(x) -> float` – square root; errors when `x < 0`.
- `Math.cbrt(x) -> float` – cube root (defined for negative `x`).
- `Math.sin(x) -> float` / `Math.cos(x) -> float` / `Math.tan(x) -> float` –
  trigonometric functions in radians.
- `Math.asin(x) -> float` / `Math.acos(x) -> float` – inverse sine/cosine;
  error unless `-1 <= x <= 1`.
- `Math.atan(x) -> float` – inverse tangent.
- `Math.atan2(y, x) -> float` – angle of `(x, y)` from the positive x-axis.
- `Math.exp(x) -> float` – `E ** x`.
- `Math.log(x) -> float` / `Math.log(x, base) -> float` – natural logarithm, or
  the logarithm in `base`; a negative operand errors and `log(0)` is
  `-Infinity`.
- `Math.log2(x) -> float` / `Math.log10(x) -> float` – base-2 and base-10
  logarithms; a negative argument errors.
- `Math.hypot(x, y) -> float` – `sqrt(x**2 + y**2)` without overflow.

Arguments outside a function's domain raise a domain error, matching Ruby's
`Math::DomainError`. Following Ruby and IEEE 754, `log(0)`/`log10(0)` return
`-Infinity` and a `NaN` or `Infinity` argument propagates through unchanged.

```vibe
Math.sqrt(9)     # 3.0
Math::PI         # 3.141592653589793
Math.hypot(3, 4) # 5.0
```

### Regex

Note the argument order: `match` takes the pattern first, while the replace
helpers take the text first.

Regex patterns are quoted strings or Ruby-style `/pattern/flags` regex
literals. A literal produces a regex value usable with the `=~` and `!~` match
operators (`"abc" =~ /b/` is the character index `1`, or `nil` when there is
no match), `case`/`when` matching, and the string pattern helpers. Supported
flags are `i` (case-insensitive) and `m` (`.` matches newlines).

- `regex.source -> string` / `regex.flags -> string` – the literal's parts.
- `regex.match(text) -> match data | nil` – match data with `captures`,
  `pre_match`, `post_match`, `begin`, and `end`, indexable with `m[0]`/`m[1]`.
- `regex.match?(text) -> bool` – whether the pattern matches.
- `Regexp.new(pattern) -> regex` / `Regexp.union(parts...) -> regex` – build
  regex values from strings.
- `Regex.match(pattern, text) -> string | nil` – first match, or `nil`.
- `Regex.replace(text, pattern, replacement) -> string` – replace the first
  match; `replacement` supports `$1` style group expansion.
- `Regex.replace_all(text, pattern, replacement) -> string` – replace every
  match.

```vibe
Regex.match("ID-[0-9]+", "ID-12 ID-34")       # "ID-12"
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1") # "X-12"
```

Patterns use Go's RE2 syntax and enforce the
[regex guard limits](#guard-limits).

### Duration

- `Duration.build(seconds) -> duration` – build from total seconds.
- `Duration.build(weeks:, days:, hours:, minutes:, seconds:) -> duration` –
  build from named parts; at least one part is required (a bare
  `Duration.build()` errors), and positional seconds and named parts are
  mutually exclusive.
- `Duration.parse(string) -> duration` – parse Go duration strings (`"1h30m"`,
  whole seconds only) or ISO 8601 durations (`"PT90S"`, `"P2W"`).

```vibe
Duration.parse("1h30m").seconds # 5400
Duration.parse("P2W").days      # 14
```

### Time

Zone keywords accept IANA names (`"America/New_York"`), `"UTC"`/`"GMT"`,
`"LOCAL"`, or numeric offsets like `"+05:30"`.

- `Time.new(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, zone = nil,
  in: nil) -> time` – build from calendar parts (local zone by default). Only the
  year is required; an omitted month or day defaults to `1` and omitted time
  fields default to midnight (`Time.new(2024)` is January 1, 2024). The seventh
  positional argument is a zone/offset that overrides `in:`
  (`Time.new(2024, 1, 1, 0, 0, 0, "+05:30")` is `+05:30`, not local).
- `Time.local(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, usec = 0) -> time` /
  `Time.mktime(...)` -> time – build calendar parts anchored to the local zone.
  Only the year is required, with the same January 1 / midnight defaults as
  `Time.new`. The seventh positional argument is microseconds-with-fraction, not
  a zone.
- `Time.utc(year, month = 1, day = 1, hour = 0, min = 0, sec = 0, usec = 0) -> time` /
  `Time.gm(...)` -> time – build calendar parts anchored to UTC. Only the year is
  required, with the same January 1 / midnight defaults (`Time.utc(2024)` is
  `2024-01-01T00:00:00Z`). The seventh positional argument is
  microseconds-with-fraction, not a zone
  (`Time.utc(2024, 1, 1, 0, 0, 0, 123456).usec` is `123456`). Integer
  microseconds are exact and floats carry sub-microsecond precision down to the
  nanosecond; a non-numeric microsecond argument raises a runtime error.
- `Time.at(epoch_seconds, subsec = nil, unit = nil, in: nil) -> time` – build
  from Unix epoch seconds (int or float). An optional subsecond value defaults to
  microseconds; an optional unit symbol (`:microsecond`/`:usec`,
  `:millisecond`, or `:nanosecond`/`:nsec`) selects the unit. A unit without a
  subsecond value, an unknown unit symbol, or a non-numeric subsecond value
  raises a runtime error; unlike `Time.utc`/`Time.local`, an explicit `nil`
  subsecond is rejected rather than treated as omitted. A fractional subsecond
  is floored toward negative infinity at nanosecond resolution, the way Ruby
  exposes it (`Time.at(0, -1.9, :nsec).nsec` is `999999998`), and subsecond
  values carry into the seconds when they exceed one second; a magnitude too
  large for the nanosecond range raises `Time.at subsecond value out of range`.
- `Time.now(in: nil) -> time` – current time (local zone by default).
- `Time.parse(string, layout = nil, in: nil) -> time` – parse a time string;
  without a layout it tries RFC3339/RFC3339Nano, RFC1123/RFC1123Z,
  `YYYY-MM-DD[THH:MM:SS]`, `YYYY-MM-DD HH:MM:SS`, `YYYY/MM/DD[ HH:MM:SS]`,
  and `MM/DD/YYYY[ HH:MM:SS]`.

## Guard Limits

JSON, regex, format, and ID helpers enforce fixed input-guard limits so hostile
data cannot exhaust host memory or CPU. The limits are not configurable
and apply to the `JSON`/`Regex`/format builtins, `String#%`, and the
regex-enabled string members (`match`, `match?`, `scan`, `sub`, `gsub`, and
their `!` variants):

| Guard | Limit |
| --- | --- |
| `JSON.parse` input / `JSON.stringify` output | 1 MiB |
| `JSON.parse` / `JSON.stringify` nesting depth | 10,000 arrays/objects |
| `format` / `sprintf` / `String#%` / `Time#strftime` output size | 1 MiB |
| Regex pattern size (`Regex.*`, `match`, `match?`, `scan`, `sub`/`gsub` with `regex: true`) | 16 KiB |
| Regex text, replacement, and output size | 1 MiB |
| `scan` match-index table (worst case) | 256 MiB |
| `random_id` length | 1024 characters |

Exceeding a limit raises a runtime error naming the offending guard.
The canonical values live in the documented const block in
`internal/runtime/limits.go`; the README's "Runtime Sandbox & Limits"
section summarizes them alongside the configurable engine quotas.

For a runnable end-to-end sample, see `examples/stdlib/core_utilities.vibe`.
