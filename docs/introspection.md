# Type tests

`value.is_type?(:atom)` tests a value's type without converting it and returns a `bool`. In a condition it narrows a local or parameter in the branch it guards, which is how a value of type `any`, such as a `JSON.parse` result, or a declared union becomes usable:

```vibe
def describe(value: any) -> string
  if value.is_type?(:int)
    "int #{value + 1}"
  elsif value.is_type?(:string)
    "string #{value.upcase}"
  elsif value.is_type?(:hash)
    "hash with #{value.length} keys"
  else
    "something else"
  end
end

describe(JSON.parse("41"))           # "int 42"
describe(JSON.parse("\"ada\""))      # "string ADA"
describe(JSON.parse("{\"a\": 1}"))   # "hash with 1 keys"
1.is_type?(:number)                  # true
"1".is_type?(:int)                   # false
```

The primitive atoms are `nil`, `bool`, `int`, `float`, `number`, `string`, `symbol`, `array`, `hash`, `range`, `duration`, `time` and `money`. A trailing `?` also accepts nil, as in `:int?`. Class and enum names resolve in the caller's lexical scope and must match the declaration's spelling exactly; an enum member matches its enum type, and an enum definition itself does not. A qualified atom such as `exports.Status` resolves an enum exported by a required module. Atoms accept at most 256 bytes; empty names, generics, unions, shapes and `any` are rejected. To check a value against a full type, including generics and shapes, use the checked cast `value.as(array<int>)` or `JSON.parse_as`, which validate at runtime and raise the typed boundary error on a mismatch.

The test takes one positional argument and no keywords or block. Name scans, type lookup and temporary frames are accounted; results do not retain their inputs, and cancellation and invocation exhaustion remain uncatchable.

`respond_to?`, `is_a?`, `kind_of?` and `instance_of?` were removed by [ADR-008](adr/008-canonical-surface-for-ai-authors.md): without inheritance the class predicates were the same test as `is_type?`, and a static type already says which members a value has. See [dispatch by name](forwarding.md) for the replacement of `respond_to?`. Until the switchover, a script compiled without static types can still call them.
