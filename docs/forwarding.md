# Dynamic method calls

`send` and `public_send` call a method named by a string or symbol. Remaining positional arguments, keywords and the attached block go to the selected method.

```vibe
class Basket
  getter items

  def initialize
    @items = []
  end

  private def append(value)
    @items.send(:push, value)
  end
end

def run(input)
  basket = Basket.new
  basket.send(:append, 3)
  basket.public_send(:items)
end
```

This returns `[3]`. `send` can reach private and protected methods. `public_send` uses normal explicit-receiver visibility: private methods reject, and protected methods require the appropriate same-class caller. Each nested forwarding helper establishes its own access rule, so `value.public_send(:send, :private_method)` can invoke the private method. Script overrides of either helper retain their own visibility and signature.

Arrays, hashes, scalars, namespaces, enums and callable builtin exports support forwarding. Plain hash entries cannot replace the universal helpers. Callable exports in namespace objects can replace them; non-callable data cannot. Raw byte strings can name callable hash entries, including names that are not valid UTF-8. A selected data property is read and then rejected as non-callable: `Time.at(0).send(:year)` does not invoke a method. Regex `source` and `flags`, array `size` and other automatic methods remain callable.

Forwarded script methods and constructors accept the reference's options-hash binding. For example, `object.send(:configure, retries: 3)` can supply `{retries: 3}` to a positional `options` parameter. Builtin exports keep their own keyword rules. Blocks retain normal `return`, `break` and `next` behavior, including the existing rule that `zip` ignores a block.

An addressed collection mutator updates its original binding while preserving other collection values. Reads use the receiver value evaluated before their arguments, even when an argument mutates the same collection. Non-mutating methods and returned temporaries do not keep that write path. Generated getters return collection values; a method can mutate the backing instance variable directly. Protected match data and rescued errors allow reads and reject writes, including forwarded writes after duplication. Typed arguments, returns and backing-field guards apply to forwarded calls.

Ordinary parentheses retain the receiver: `(items.send)(:push, 2)` updates `items`. Helpers selected through `rescue` follow the existing [computed-call rules](computed-calls.md). Safe navigation skips method-name arguments, remaining arguments and blocks when the receiver is nil.

Nested forwarding uses an iterative lookup loop. It accounts for name scans and argument traversal without consuming a VM recursion frame per helper or repeatedly shifting the argument list. Diagnostics preserve raw method-name bytes and charge their storage. Cancellation, deadlines and exhausted limits remain latched. Tests cover exact work and memory quotas, long forwarding chains, mutation paths and reclamation after failed calls.

The [reference differences](forwarding-differences.json) record six cases following the selected match-data policy, two cases retaining Rust's existing typed nested-write guards, and three cases of an ordinary-call options-hash bug still to fix. Twelve further cases follow the selected collection snapshot semantics when arguments mutate a forwarded receiver. Forty-one additional cases have the same error class with different diagnostic wording. These are separate from the 1,107 shared forwarding and lookup evaluations and 74 uncaught runtime rejection cases.
