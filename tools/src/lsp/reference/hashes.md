# Hashes

A hash maps string keys to values and keeps its entries in insertion order.
Vibescript has two kinds of hash type:

- A **shape** is a record with a fixed set of fields, such as
  `{ name: string, age: int }`. Every hash literal is a shape, and its fields
  are read with literal keys.
- A **dictionary**, `hash<string, V>`, holds any number of keys that are known
  only at runtime, all with values of type `V`.

```vibe
player = { id: "p1", name: "Alex", raised: money("25.00 USD") }
player["name"]                          # "Alex", a string

counts: hash<string, int> = {}          # an empty dictionary needs a declared type
counts["alex"] = 3
counts["maya"] = 5
counts.length                           # 2
```

## Keys

Keys are strings. A label such as `name:` in a literal is the string key
`"name"`, and a quoted label such as `"first-name":` writes a key that is not an
identifier. A label followed directly by `,` or `}` takes its value from the
local of the same name. Reserved words are ordinary labels:

```vibe
name = "Ada"
role = "engineer"
person = { name:, role: }               # { name: "Ada", role: "engineer" }
contact = { "first-name": "Ada", "last name": "Lovelace" }
contact["first-name"]                   # "Ada"
result = { begin: 0, rescue: "retry" }
result["rescue"]                        # "retry"
```

A hash is read with `h["key"]` only; a dot always calls a member, so a field
named like a member, such as `length`, never hides it. A key computed at runtime
is written into a dictionary after it is built, and any other key type is a
compile error:

```vibe
status = "active"
tally: hash<string, int> = {}
tally[status] = 1
tally[1.to_s] = 2                       # keys are strings; convert other values
```

An overwritten key keeps its original position, and every transform keeps the
receiver's order. Since keys are strings, a JSON round trip reads the same
entries:

```vibe
person = { name: "Ada" }
JSON.parse(JSON.stringify(person)) == person   # true
```

## Shapes

A shape's fields are known: reading a declared field with a literal key gives
the field's type, not an optional one. An optional field, written `age?: int`,
may be absent and reads as `int?`; `age: int?` is a field that is always
present but may hold `nil`. A `type` alias names a shape:

```vibe
type Player = { name: string, nickname?: string }

def label(player: Player) -> string
  nickname = player["nickname"]         # string?
  if nickname != nil
    return nickname
  end
  player["name"]                        # string
end

label({ name: "Ada" })                  # "Ada"
label({ name: "Ada", nickname: "A" })   # "A"
```

A record is not a dictionary. Reading or writing a field a shape does not
declare is an error, and so is indexing it with a key known only at runtime:

```vibe error=V0110
player = { id: "p1", name: "Alex" }
player["email"]
```

```vibe error=V0111
player = { id: "p1", name: "Alex" }
field = "name"
player[field]   # the fix declares `player: hash<string, string>`
```

A shape whose fields all have type `V` is assignable to `hash<string, V>`, so a
record can be passed where a dictionary is expected:

```vibe
def describe(fields: hash<string, string>) -> string
  fields.keys.join(", ")
end

describe({ id: "p1", name: "Alex" })    # "id, name"
```

Shapes answer the dictionary members below, with `V` the union of their field
types: `{ id: "p1", score: 3 }.values` is `array<int | string>`.
`JSON.parse_as(text, { name: string })` checks parsed JSON against a shape and
gives a value of that type.

## Values, not references

Hashes are values. Binding, passing or returning a hash produces another value,
and an update through one name is never visible through another. An update
such as `h["key"] = value` or `delete` changes the local, instance variable or
literal-key field it names. A shape field read with a literal key is a path, so
`order["items"] << item` updates `order`. A dictionary read is optional, so a
nested value is read, updated and stored back:

```vibe
order = { id: "o-1", items: [1] }
order["items"] << 2                     # order is { id: "o-1", items: [1, 2] }

totals: hash<string, hash<string, int>> = { week: { visits: 0 } }
week = totals.fetch("week")
week["visits"] = 1
totals["week"] = week
```

## Reading

`h[key]` on a dictionary has type `V?` and reads `nil` for a missing key. Use
`fetch` when the key must be present, or test the value with `!= nil`.

### `fetch(key: string, default?: V, &block?: string -> V) -> V`

The value for `key`. A missing key returns `default`, or the block's result for
the key, and raises when neither is given. `fetch` never stores anything.

### `fetch_values(*keys: array<string>, &block?: string -> V) -> array<V>`

The values for several keys, in the order requested. A missing key raises, or
takes the block's result for it.

### `values_at(*keys: array<string>) -> array<V?>`

The values for several keys, in the order requested, with `nil` for a missing
key.

### `dig(key: string, *path: array<string | int>) -> any`

Walks nested hashes and arrays: a string reads a hash key and an integer
indexes an array. A missing key or index gives `nil`. The result is `any`, so
narrow it before use.

### `key?(key: string) -> bool`

Whether the hash has an entry for `key`, even one whose value is `nil`.

### `value?(value: V) -> bool`

Whether some entry's value is equal to `value`. Equality compares contents, and
an int never equals a float.

### `keys -> array<string>`

The keys in order.

### `values -> array<V>`

The values in key order.

### `length -> int`

The number of entries.

### `empty? -> bool`

Whether the hash has no entries.

```vibe
labels: hash<string, string> = { home: "Home", away: "Away" }
labels["draw"]                             # nil
labels.fetch("home")                       # "Home"
labels.fetch("draw", "?")                  # "?"
labels.fetch("draw") { |key| key.upcase }  # "DRAW"
labels.values_at("away", "draw")           # ["Away", nil]
labels.fetch_values("home", "away")        # ["Home", "Away"]
labels.key?("home")                        # true
labels.value?("Away")                      # true
labels.keys                                # ["home", "away"]

payload: hash<string, any> = { profile: { total: 12 } }
payload.dig("profile", "total")            # 12, as any
```

## Iteration

### `each(&block: (string, V)) -> hash<string, V>` / `each(&block: [string, V]) -> hash<string, V>`

Runs the block for each entry and returns the receiver. A block with two
parameters gets the key and the value; a block with one gets the `[key, value]`
pair as a tuple.

### `each_key(&block: string) -> hash<string, V>`

Runs the block for each key.

### `each_value(&block: V) -> hash<string, V>`

Runs the block for each value.

### `each_with_index(&block: ([string, V], int)) -> hash<string, V>`

Runs the block with each `[key, value]` pair and its index.

### `map<U>(&block: (string, V) -> U) -> array<U>` / `map<U>(&block: [string, V] -> U) -> array<U>`

An array of the block's results for each entry, taking the key and value or
the pair.

### `map_with_index<U>(&block: ([string, V], int) -> U) -> array<U>`

An array of the block's results for each pair and its index.

### `to_a -> array<[string, V]>`

The entries as `[key, value]` pairs. `to_h` on an array of pairs turns them
back into a hash, so sorting a hash goes through `to_a`.

A `for` loop over a hash binds each `[key, value]` pair in turn.

```vibe
scores: hash<string, int> = { ada: 3, bo: 1 }
lines: array<string> = []
scores.each { |name, score| lines << "#{name}=#{score}" }
lines                                          # ["ada=3", "bo=1"]
scores.map { |pair| pair[0] }                  # ["ada", "bo"]
scores.to_a                                    # [["ada", 3], ["bo", 1]]
ranked = scores.to_a.sort_by { |pair| pair[1] }
ranked.to_h                                    # {bo: 1, ada: 3}

total = 0
for pair in scores
  total = total + pair[1]
end
total                                          # 4
```

## Transforming

These members return a new hash and leave the receiver alone.

### `select(&block: (string, V) -> bool) -> hash<string, V>`

The entries the block accepts.

### `reject(&block: (string, V) -> bool) -> hash<string, V>`

The entries the block does not accept.

### `transform_values<U>(&block: V -> U) -> hash<string, U>`

The same keys with the block's result for each value.

### `transform_keys(&block: string -> string | symbol) -> hash<string, V>`

The same values under the block's key for each key; a symbol result is stored
as its string. When two keys map to one, the later entry wins.

### `deep_transform_keys(&block: string -> string | symbol) -> hash<string, V>`

Like `transform_keys`, also renaming the keys of hashes nested in the values,
including hashes inside arrays.

### `remap_keys(mapping: hash<string, string | symbol>) -> hash<string, V>`

Renames the keys `mapping` names and keeps the others.

### `merge(*others: array<hash<string, V>>, &block?: (string, V, V) -> V) -> hash<string, V>`

The receiver with the other hashes' entries added in order. A later hash wins a
key present in both, unless the block resolves it from the key, the old value
and the new one. Without arguments it copies the receiver.

### `slice(*keys: array<string>) -> hash<string, V>`

Only the entries for the given keys; keys that are absent are skipped.

### `except(*keys: array<string>) -> hash<string, V>`

Every entry except those for the given keys.

### `compact -> hash<string, V>` on `hash<string, V?>`

The entries whose value is not `nil`.

### `flatten(depth: int = 1) -> array<any>`

The entries as one array, `[key, value, key, value, ...]`. A depth above 1 also
flattens array values that many levels further, `0` gives the pairs, and a
negative depth flattens completely.

```vibe
stock: hash<string, int> = { apples: 3, pears: 0, plums: 7 }
stock.select { |name, count| count > 0 }       # {apples: 3, plums: 7}
stock.reject { |name, count| count > 0 }       # {pears: 0}
stock.transform_values { |count| count * 2 }   # {apples: 6, pears: 0, plums: 14}
stock.transform_keys { |name| name.upcase }    # {APPLES: 3, PEARS: 0, PLUMS: 7}
stock.slice("apples", "kiwis")                 # {apples: 3}
stock.except("pears")                          # {apples: 3, plums: 7}
stock.merge({ pears: 5 })                      # {apples: 3, pears: 5, plums: 7}
stock.merge({ pears: 5 }) { |name, old, new| old + new }  # pears is 5

names: hash<string, string> = { first_name: "Alex" }
names.remap_keys({ first_name: "name" })       # {name: "Alex"}

nested: hash<string, any> = { player_id: 7, profile: { total_raised: 12 } }
nested.deep_transform_keys { |key| key.upcase }  # {PLAYER_ID: 7, PROFILE: {TOTAL_RAISED: 12}}

maybe: hash<string, int?> = { a: 1, b: nil }
maybe.compact                                  # {a: 1}
```

## Updating

`h[key] = value` stores an entry, adding a new key at the end. These members
also change the hash they are called on (see [Values, not
references](#values-not-references)).

### `delete(key: string, &block?: string -> V) -> V?`

Removes the entry and returns its value. On a miss it returns `nil`, or the
block's result for the key, and leaves the hash unchanged.

### `delete_if(&block: (string, V) -> bool) -> hash<string, V>`

Removes the entries the block accepts and returns the hash. The block sees the
entries as they were when the call began.

### `keep_if(&block: (string, V) -> bool) -> hash<string, V>`

Keeps only the entries the block accepts and returns the hash.

### `replace(other: hash<string, V>) -> hash<string, V>`

Replaces every entry with those of `other`.

### `clear -> hash<string, V>`

Removes every entry.

```vibe
cart: hash<string, int> = { apples: 2, pears: 1, plums: 4 }
cart["kiwis"] = 3
cart.delete("pears")                           # 1
cart.delete("figs")                            # nil
cart.delete_if { |name, count| count > 3 }     # {apples: 2, kiwis: 3}
cart.keep_if { |name, count| count < 3 }       # {apples: 2}
cart.replace({ figs: 1 })                      # {figs: 1}
cart.clear                                     # {}
```

## Debug representation

`inspect` renders a hash with label keys, quoting a key that is not an
identifier and inspecting each value, in insertion order:

```vibe
{ a: 1, b: "x" }.inspect      # "{a: 1, b: \"x\"}"
{ "with space": 1 }.inspect   # "{\"with space\": 1}"
```

## Removed spellings

These do not compile; the compiler reports each with its replacement.

- `size` – removed; use `length`.
- `has_key?`, `member?` and `include?` – removed; use `key?`.
- `has_value?` – removed; use `value?`.
- `store(key, value)` – removed; use `h[key] = value`.

Symbol keys such as `h[:name]` are written `h["name"]`, a field read with a dot
such as `h.name` is written `h["name"]`, and `Hash.new` is `{}` with a declared
type, such as `counts: hash<string, int> = {}`.
