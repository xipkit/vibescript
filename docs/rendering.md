# Inspection, templates and projections

`inspect` returns a debug string for core values. Arrays and hashes inspect their nested values; hashes keep insertion order and namespace objects list fields in key order. Strings are quoted, symbols have a leading colon, and nil renders as `nil`.

```vibe
data = { name: "Ada", tags: [:ready, nil] }
data.inspect
```

The result is `{name: "Ada", tags: [:ready, nil]}`. Inspection escapes backslashes, double quotes, newlines, tabs and literal interpolation markers. Other bytes, including invalid UTF-8, remain unchanged. Quoted inspection is separate from type literals' canonical field quoting. Nested type literals render their canonical form; direct `shape.inspect` remains unsupported. `inspect` takes no arguments, keywords or block.

`template(context: hash<string, any>, *, strict: bool = false) -> string` substitutes scalar values from a hash. Dots in a placeholder traverse nested hash fields; they do not call methods or index arrays. Missing values keep the original placeholder unless `strict: true` requests an error. Invalid placeholder syntax stays literal in either mode.

```vibe
"Hello {{user.name}}! {{missing}}".template({ user: { name: "Ada" } })
```

The result is `Hello Ada! {{missing}}`. Placeholder names begin with an ASCII letter or underscore; subsequent bytes can also be digits, dots or hyphens. Spaces, tabs, newlines, form feeds and carriage returns can surround the name. Nil substitutes an empty string, symbols use their names, and enum members use their normalized symbols. Collections, type literals and methods cannot substitute for scalars. Repeated placeholders reuse their formatted value, including when a template has more than eight distinct names.

Array and hash `values_at` return values in selector order, preserving duplicates and using nil for missing entries, so their elements are optional: `array<T?>` and `array<V?>`. Hash keys are strings; array selectors are integers or ranges, and negative indexes count from the end.

```vibe
[10, 20, 30].values_at(2, 0..1, 8)
```

The result is `[30, 10, 20, nil]`. Array ranges expand their selected positions and pad indexes past the end with nil. Beginless ranges start at zero; endless ranges continue through the last element. Descending windows are empty. A range starting before the array's first position after negative-index normalization is an error. No selectors produce an empty array.

Projection and rendering preserve collection value semantics. New output, cached scalar text and ordering buffers are accounted while their inputs remain live. Output expansion is checked before allocation, and scans, conversions and emitted positions participate in work and cancellation checks. These methods have no additional fixed payload cap; the call's limits govern them.
