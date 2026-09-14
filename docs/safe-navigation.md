# Safe navigation

`receiver&.member` reads a member or calls a method when the receiver is not nil. A nil receiver produces nil and skips the member lookup, arguments, keywords, splats and attached block. False remains an ordinary receiver.

```vibe
def greeting(user)
  user&.name&.upcase
end

def run(input)
  [greeting({name: "Ada"}), greeting(nil), false&.nil?]
end
```

This returns `["ADA", nil, false]`. The receiver expression runs once. Errors, cancellation and exhausted limits during its evaluation still propagate.

Each `&.` guards its immediate access. `user&.profile.name` still reads `.name` from the intermediate result, which can be nil. Use `user&.profile&.name` to guard both accesses. Safe calls support parentheses, calls without parentheses, keywords, splats and synchronous blocks. Physical newlines can wrap member accesses; semicolons still separate statements.

Safe navigation is rejected in member, call and index receiver chains used as assignment targets. For example, `user&.name = "Ada"`, `user&.items[0] = 1` and `user&.count += 1` are syntax errors. The same restriction applies to destructuring. An independent safe expression can still compute an index or argument.

Mutating methods on non-nil receivers use the same binding and path rules as ordinary calls. `items&.push(1)` updates `items` when it is an array, and leaves earlier collection snapshots unchanged. Match-data protection also remains enforced: a non-nil match cannot be changed through `match&.captures.push("x")`, and its duplicates retain protection and rendering.

Execution still accounts for work and memory and enforces recursion, cancellation and deadlines. Skipped arguments and blocks consume no execution storage of their own.
