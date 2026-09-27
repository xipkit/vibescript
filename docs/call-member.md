# Calls to a member named call

`.call(...)` is an ordinary method call on a typed receiver. A class may define it with the same parameter, keyword, result and block rules as any other method:

```vibe
class Prefix
  @prefix: string

  def initialize(@prefix: string)
  end

  def call(text: string) -> string
    "#{@prefix}: #{text}"
  end
end

Prefix.new("notice").call("ready") # "notice: ready"
```

Functions and builtins are not values: call `helper(1)` directly, rather than trying to obtain a function object and invoke `.call`. Hash fields cannot hold callable values. The checker rejects unknown members and non-callable data before execution.

Safe navigation on a nil receiver skips arguments and the block. A script-defined `call` retains ordinary visibility, argument binding, block control flow and resource accounting. See [computed calls](computed-calls.md) and [host capabilities](capabilities.md).
