# Computed calls

In the static language a call target is a name: a function, a method on a typed receiver, a namespace member or a host function or capability the host declares. A parenthesized expression, a value read from a collection or a `rescue` modifier cannot be called (V0310), and a name that is not in scope is a compile error (V0201), so every call is checked against its signature. Choose between functions with `if` or `case` instead:

```vibe
def primary(value: int) -> int
  value + 1
end

def fallback(value: int) -> int
  value - 1
end

def run(use_primary: bool) -> int
  if use_primary
    primary(41)
  else
    fallback(41)
  end
end
```

Script functions remain confined to call syntax, as required by ADR-006: they are never values.

Explicit calls to a member named `call` also resolve their target before evaluating arguments. See [member call selection](call-member.md).
