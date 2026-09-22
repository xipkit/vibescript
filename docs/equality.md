# Equality predicates

`eql?(other)` compares values of the same kind. Its kind check applies at every collection depth: `1.eql?(1.0)` and `[1].eql?([1.0])` are both false. Arrays and hashes compare their contents; hash insertion order does not change equality.

Ordinary `==` and collection `equal?` compare integers and floats exactly. For example, `9007199254740993 == 9007199254740992.0` is false; converting the integer to float first would incorrectly erase that difference. This equality rule also applies inside arrays and hashes.

`equal?(other)` uses content equality for arrays and hashes, so `[1].equal?([1.0])` is true. Scalars require the same kind and value, with these identity rules:

- Separately produced large integers have distinct identities. An alias, `dup`, `clone`, `freeze` or unary plus preserves the existing payload identity.
- Any two NaN floats are `equal?`; NaN remains unequal under `==` and `eql?`. A collection containing NaN still uses ordinary content equality.
- Class instances compare by instance identity. Classes and modules compare by their declaration identity.
- Enum definitions and members have identity in addition to their nominal `==`/`eql?` comparison. Repeated reads of the same member share that identity.

Scoped builtin exports and match-offset callables can also be compared directly or through immediate wrapped calls, such as `JSON::parse.equal?(JSON::parse)` or `(match[:begin].eql?)(match[:begin])`. Ordinary executable-value restrictions still apply when reading local variables.

Class overrides take precedence. Plain hash data named `eql?` or `equal?` does not replace a predicate; callable namespace exports can replace it. Match data and errors retain their protection and special rendering.

Addressable mutation chains still resolve stored fields: `h.eql?.clear` clears the stored array in `h = {"eql?": [1]}`. Parentheses select the method explicitly; `h.eql?().clear` raises an argument error and leaves the hash intact.

Predicates accept one positional argument and reject keywords and blocks. The existing `duration.eql?` and `time.eql?` methods ignore a supplied block. Invalid calls do not execute that block. Unlike equality operators, these method calls enforce their own argument contracts.

An immediate wrapped call captures the receiver before evaluating its arguments:

```vibescript
a = [1]
result = (a.eql?)(a.push(2))
[result, a] # [false, [1, 2]]
```

Detached predicates such as `probe = a.eql?` are rejected under ADR-006. Older Go helper comments describe internal bound builtins; executable reference probes confirm that those comments do not permit general stored method values in scripts. Existing scoped callable exports retain their documented [computed-call behavior](computed-calls.md).

Incoming enum arguments from the same compiled script resolve to that invocation's enum identity, including references inside collections and instance fields. Foreign enum aliases preserve their shared identity through independent accounting views. Callback results retain their source identity: a previously returned member can be `eql?` to the current script member without being `equal?` to it. Identity tokens do not retain the originating invocation's accounting state.

Comparisons charge visited values, string bytes and hash lookups. Within one comparison, `==`, `eql?`, `<=>` and sorting walk each pair of shared arrays or hashes once: a pair reached again along another path reuses its recorded result, so structures built from shared parts compare in time proportional to their distinct pairs rather than their unfolded size. Recorded pairs are charged and reserved against the memory limit, and are released when the comparison finishes. Wrapped receivers use the existing accounted argument storage and are released on completion or unwinding. Enum tokens and their temporary lookup cache are reserved before allocation; imports charge metadata independently. Exact step and memory limits, cancellation and uncatchable invocation exhaustion remain enforced.

Array `uniq`, `union`, `difference`, `&` and `-` index their keys in a hash table reserved against the call's memory limit, so they take time and steps proportional to their inputs. Keys hash consistently with the set-key rules above: numbers by exact value, times by instant and hashes independently of insertion order. Long strings, arrays and large integers hash bounded samples; equal hashes still compare by ordinary equality, and every probe and comparison is charged.
