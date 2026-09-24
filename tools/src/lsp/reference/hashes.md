# Hashes in Vibescript

Hashes are ordered dictionaries with one string keyspace. Strings and symbols
are the only accepted keys, and a symbol normalizes to its string, so
`hash["name"]`, `hash[:name]`, and the label `name:` all address one entry.
Declare identifier-shaped keys with Ruby-style shorthand:

```vibe
player = {
  id: "p1",
  name: "Alex",
  raised: money("25.00 USD"),
}
```

Shorthand labels (`name:`) create string keys, so `player[:name]` and
`player["name"]` read the same entry and `player.keys` returns `["id", "name",
"raised"]`.

When a label key is followed immediately by `,`, `}`, or end-of-input, the value
is omitted and read from the local variable of the same name. `{ name: }` is
shorthand for `{ name: name }`, which mirrors the call-site keyword shorthand
(`greet name:`):

```vibe
name = "Ada"
role = "engineer"
{ name:, role: } # { name: "Ada", role: "engineer" }
```

Value omission applies only to label keys; quoted keys such as `{ "name": }`
have no matching local and are rejected. If the named local is not defined, the
hash literal reports the usual undefined-variable error.

Use quoted keys for JSON-shaped payloads or names that are not valid
identifiers:

```vibe
player = {
  "first-name": "Ada",
  "last name": "Lovelace"
}

player["first-name"]  # "Ada"
player[:"first-name"] # "Ada"  (the same key)
```

A quoted key and its symbol spelling name one entry, so a literal that writes
both keeps one entry holding the later value.

Reserved words are valid shorthand labels when followed by an explicit value, so
keyword-shaped payload keys behave like any other label:

```vibe
result = { begin: 0, rescue: "retry", ensure: true, raise: false, return: 1 }
result[:rescue] # "retry"
```

This mirrors Ruby, which treats keyword-shaped labels uniformly. The same labels
are accepted as keyword arguments at call sites, with or without parentheses
(`record(rescue: "retry")` and `record rescue: "retry"`). The trailing colon
disambiguates the label from the keyword, so `record rescue: "retry"` passes a
keyword argument rather than parsing `rescue` as a control-flow keyword.

Hash literals only accept colon-style keys: shorthand labels (`name:`) and
quoted string keys (`"name":`). Ruby's hash rocket syntax (`=>`) is not
supported, so write `{ name: "Ada" }` rather than `{ :name => "Ada" }`. To use
a value computed at runtime as a key, assign into the hash after constructing
it:

```vibe
current_key = :nickname
player = { name: "Ada" }
player[current_key] = "dynamic"
```

A key must be a string or a symbol wherever one enters or probes a hash --
index assignment, `store`, `[]`, `fetch`, `key?`, `delete`, `dig`, `slice`,
`except`, `values_at`, the merge family, `transform_keys`, `Array#to_h`,
`group_by`, and `tally`. Anything else raises `unsupported hash key type ...:
hash keys must be strings or symbols; convert the key with to_s`. Convert a
computed key explicitly:

```vibe
numbers = {}
numbers[1.to_s] = "one"        # "1"
numbers[[2, 3].to_s] = "pair"  # "[2, 3]"
```

Because keys are strings, a JSON round trip does not change lookup behavior:

```vibe
person = { name: "Ada" }
JSON.parse(JSON.stringify(person)) == person # true
```

Dot access keeps hash method names reserved. If a stored key is named like a
hash method, use index access for the entry:

```vibe
sizes = { size: "XL" }
sizes.size   # 1
sizes[:size] # "XL"
```

## Missing keys

A missing key reads as `nil`. Hashes carry no per-hash default: `Hash.new` takes
no argument and no block, so a fallback is written where it is needed rather
than stored on the hash.

```vibe
counts = {}
counts[:misses]              # nil
counts.fetch(:misses, 0)     # 0   (an explicit fallback, per lookup)
counts.fetch(:misses) { 0 }  # 0   (computed only on a miss)
counts.size                  # 0   (fetch never inserts)
```

`fetch` with no fallback raises for a missing key, which is the way to demand a
key be present:

```vibe
{ a: 1 }.fetch(:a)       # 1
{ a: 1 }.fetch(:missing) # error: hash.fetch key not found: :missing
```

To fill a hash lazily, write the store where the miss is handled:

```vibe
cache = {}
key = "a"
cache[key] = "made-" + key if cache[key].nil?
```

`dig` and `values_at` read as `[]` does: a missing key contributes `nil` rather
than consulting anything stored on the hash.

```vibe
{}.dig(:missing)              # nil
{ a: 1 }.values_at(:a, :b)    # [1, nil]
```

## Query helpers

- `size` / `length`
- `empty?`
- `key?`, `has_key?`, `member?`, `include?` for key membership.
- `value?`, `has_value?` for value membership.

```vibe
def has_required_fields(player)
  player.key?(:name) && player.include?("raised") && !player.empty?
end
```

The key membership predicates use the same key identity as `[]` lookup, and
reject a candidate that is neither a string nor a symbol.

```vibe
{ a: 1 }.key?(:a)  # true
{ a: 1 }.key?("a") # true  (the same key)
{ a: 1 }.key?(1)   # error: hash keys must be strings or symbols
```

`value?` and `has_value?` compare the candidate against each stored value using
the same `==` equality as the rest of Vibescript, so deep collections match by
content and integers do not match equal-looking floats.

```vibe
{ a: 1, b: 2 }.value?(2)        # true
{ a: [1, 2] }.has_value?([1, 2]) # true
{ a: 1 }.value?(1.0)            # false
```

## Access helpers

- `fetch(key, default)` returns the value for `key`. Like Ruby, a missing key is
  treated as exceptional: when no `default` argument and no block are supplied,
  `fetch` raises `key not found`. Supply a `default` to return instead of
  raising, or pass a block to compute the fallback from the requested key. When
  both a `default` and a block are supplied, the block supersedes the default and
  is evaluated on a miss, matching Ruby. Use `[]` or `dig` when a missing key
  should yield `nil` rather than raise.
- `fetch_values(*keys)` returns the values for several keys at once, in the
  requested order. Like `fetch`, it raises when a key is absent. Pass a block
  to compute a replacement for each missing key instead of raising.
- `dig(*path)` for nested lookup. A path component descends one level: a
  symbol or string key into a hash, or an integer index into an array, so a
  single `dig` can walk JSON-shaped data that mixes hashes and arrays. Each hash
  step is a `[]` lookup, so a missing key yields `nil`. Out-of-range array
  indexes also yield `nil`. Indexing an array with a non-integer component
  raises, matching how arrays reject non-integer indexes elsewhere.
- `values_at(*keys)` to read several values at once, in requested key order. Each
  key is a `[]` lookup, so a missing key yields `nil`.

```vibe
def display_name_or_default(records, player_id)
  fallback = "unknown"
  records.dig(player_id, :meta, :display_name) || fallback
end

{ a: [10, 20] }.dig(:a, 1)  # 20
{ a: { b: 1 } }.dig(:a, :z) # nil
```

```vibe
{ a: 1, b: 2 }.values_at(:b, :c, :a)           # [2, nil, 1]
{ a: 1, b: 2 }.fetch(:b)                        # 2
{ a: 1 }.fetch(:missing)                        # raises "key not found: :missing"
{ a: 1 }.fetch(:missing, 99)                    # 99
{ a: 1 }.fetch(:missing) { |k| k }             # :missing
{ a: 1 }.fetch(:missing, 99) { |k| k }         # :missing (block supersedes default)
{ a: 1, b: 2 }.fetch_values(:a, :b)            # [1, 2]
{ a: 1 }.fetch_values(:a, :missing)            # raises "key not found: :missing"
{ a: 1 }.fetch_values(:a, :missing) { |k| k }  # [1, :missing]
```

## Transform and filter helpers

- `merge(*others)` combines the receiver with any number of hashes into a new
  hash. Later hashes win on key conflicts, and an optional block resolves
  conflicts by yielding `(key, old_value, new_value)`. Called with no arguments
  it returns a copy of the receiver.
- `replace(other)` discards the receiver's entries and adopts `other`'s,
  updating the local, instance variable, or nested path the call names. It
  does not carry default metadata: hashes have none.
- `flatten(depth = 1)` returns a flat array of the entries. At the default depth
  the result is `[key, value, ...]`; values that are arrays are kept nested
  unless a deeper `depth` is given. A `depth` of `0` returns the `[key, value]`
  pairs nested, and a negative `depth` flattens completely. Because a deep
  flatten's output length cannot be bounded before walking the values, both the
  `[key, value]` pair materialization and the flattened result are charged
  against the step and memory quotas as they are built, so an oversized flatten
  is rejected mid-build instead of after the full result materializes.
- `store(key, value)` is Ruby's method spelling of index assignment: it writes
  the entry into the receiver in place and returns the stored value. An
  existing key keeps its position in the insertion order.
- `delete(key)` / `delete(key) { |key| default }` removes the entry from the
  receiver in place and returns the removed value, matching Ruby's mutating
  `delete`. On a miss it returns `nil` — or, with a block, the block's result
  for the requested key — and leaves the receiver untouched.
- `clear` empties the receiver it names and returns it.
- `delete_if { |key, value| }` removes every entry the block accepts and
  returns the receiver; `keep_if { |key, value| }` keeps only accepted entries.
  The block sees a snapshot of the entries, so the receiver is pruned only
  after the walk completes; surviving entries keep their insertion order.
- `compact` removes `nil` values.
- `slice(*keys)` keeps only selected keys. Absent candidates are omitted, so
  `slice` with only unmatched candidates returns an empty hash; a candidate that
  is not a string or symbol is rejected.
- `except(*keys)` removes selected keys. Absent candidates are ignored; a
  candidate that is not a string or symbol is rejected.
- `select` / `reject` with a block.
- `transform_keys` / `transform_values` with a block.
- `deep_transform_keys` for recursive key mapping across nested hashes/arrays.
- `remap_keys(mapping_hash)` for direct key rename maps.

The map-producing transforms run inside the sandbox. Before building a derived
map (or growing the receiver, for `replace`) they project its size against the
memory quota, so a transform over a large
hash is rejected up front rather than after the backing map is allocated. While
walking entries they charge the step quota per entry and honor context
cancellation, so large materializations stay bounded. This applies to `merge`,
`replace`, `compact`, `slice`,
`except`, `select`, `reject`, `transform_keys`, `transform_values`, and
`remap_keys`. The in-place `store`, `delete`, and `clear` allocate nothing
beyond the single written entry, so they carry no per-entry walk.

The block-driven transforms (`transform_keys`, `transform_values`, and the
`merge` conflict block) also charge what a block produces against the memory quota
as it is produced, so fresh content accumulated in the result cannot exceed the
quota before the build completes. `transform_values` and the `merge` conflict
block charge each block-returned *value* at its full payload; `transform_keys`
charges each block-synthesized *key* (its value stays a receiver value already
counted, so only the fresh key is new). This block-result charge is
*conservative*: each result is counted as it is inserted, deduplicated only
against other block results and never against the receiver or argument values --
those are already counted once in the call's live footprint, so they are written
to the output map slots without being re-measured. A block that returns a value
unchanged and shared with the receiver -- or that collapses several writes onto
one key -- is therefore counted at full size rather than deduplicated away. This
over-count is deliberate: it keeps the bound sound even when a block mutates a
receiver-owned container in place (for example appending into an array that still
has spare capacity) and returns it, a case where deduplicating against the
receiver would charge the fresh payload nothing and let it escape the quota. The
array-side equivalent of this accounting is tracked in #787.

Two helpers are not yet bounded this way:

- `deep_transform_keys` does not bound its recursive materialization against the
  sandbox limits (tracked in #786).
- `flatten` materializes an insertion-ordered key list and a `[key, value, ...]` array
  without a projected memory check or per-entry step charge; it is grouped with
  the array-materialization work alongside `keys` and `values`.

Apply both only to inputs of known size.

```vibe
def public_profile(record)
  record
    .slice(:name, :raised, :goal)
    .reject { |key, value| value == nil }
end
```

```vibe
payload = { player_id: 7, profile: { total_raised: 12 } }
payload.deep_transform_keys do |k|
  if k == "player_id"
    "playerId"
  elsif k == "total_raised"
    "totalRaised"
  else
    k
  end
end

{ first_name: "Alex" }.remap_keys({ first_name: :name })
```

## Iteration helpers

- `keys` and `values`
- `each`, `each_key`, `each_value`
- `to_a` returns the `[key, value]` pairs as a nested array, with keys exposed as
  strings. It is the inverse of `Array#to_h` and equivalent to `flatten(0)`. The
  materialization charges its output (the pair arrays and the key scratch)
  against the memory quota as the pairs accumulate, and charges the step quota per
  pair while honoring context cancellation, so a large hash stays bounded rather
  than allocating the whole nested array before the runtime can reject it.

```vibe
{ a: 1, b: 2 }.to_a # [["a", 1], ["b", 2]]
```

- `each_with_index` yields each entry's `[key, value]` pair (keys exposed as
  strings) plus its 0-based index and returns the receiver. Matching Ruby's
  `Hash#each_with_index`, the pair is the first block parameter and the index the
  second, so `{ b: 2, a: 1 }.each_with_index { |pair, index| ... }` yields
  `(["b", 2], 0)` then `(["a", 1], 1)`.
- `map_with_index` yields the same `[key, value]` pair plus index and collects
  each block result into a new array (`{ b: 2, a: 1 }.map_with_index { |pair, index| [pair[0], index] }`
  is `[["b", 0], ["a", 1]]`). It takes no arguments and requires a block.

`keys`, `values`, `flatten`, `to_a`, and block-based hash iteration process
entries in Ruby-style insertion order: a hash keeps the order its entries were
first inserted, an overwritten key keeps its original position, and transforms
preserve their receiver's order. Iteration is deterministic because the order is
stored with the hash rather than derived from Go map storage. Hashes that reach
the runtime as bare Go maps (host-provided values and keyword-argument splats)
carry no insertion record and iterate in sorted key order instead.

`each` yields each entry following Ruby's block-argument rules. A block with a
single parameter receives the entry as a two-element `[key, value]` pair, while a
block with two parameters receives the key and value separately. Extra parameters
receive `nil`, and a single destructuring parameter such as `|(key, value)|`
unpacks the pair:

```vibe
pairs = []
{ a: 1, b: 2 }.each do |pair|
  pairs.push(pair)
end
# pairs == [["a", 1], ["b", 2]]

entries = []
{ a: 1, b: 2 }.each do |key, value|
  entries.push([key, value])
end
# entries == [["a", 1], ["b", 2]]
```

A `for` loop may also iterate a hash directly, mirroring Ruby's loop over
`each`. Each iteration binds a two-element `[key, value]` pair (keys exposed as
strings), visited in the same insertion order:

```vibe
def entries(hash)
  out = []
  for pair in hash
    out = out + [pair]
  end
  out
end
# entries({ b: 2, a: 1 }) => [["b", 2], ["a", 1]]
```

Example:

```vibe
def merge_defaults(player)
  defaults = { goal: 5000, raised: money("0.00 USD") }
  defaults.merge(player)
end
```

## Debug Representation

`inspect` renders a hash as a parseable debug string using Vibescript's
colon-label key form (not Ruby's unsupported hash-rocket syntax), inspecting each
value recursively:

```vibe
{ a: 1, b: "x" }.inspect    # => "{a: 1, b: \"x\"}"
{ "with space": 1 }.inspect # => "{\"with space\": 1}"
```

Inspected entries follow the hash's insertion order, so the rendering is stable
across calls and matches iteration. Bare host-provided maps
(`NewHash(map[string]Value)`) carry no insertion record, so — like their
iteration — their rendered order is unspecified rather than insertion-ordered.
See [Debug Representation](stdlib_core_utilities.md#debug-representation) for the
full per-kind contract.

Review `examples/hashes/` for live scripts used by the tests.
