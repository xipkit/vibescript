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

Static types never compute a call target, since functions are not values. The runtime can still select one for the ADR-004 language that `vibes migrate` compiles, until that support is removed: by a `rescue` modifier, as in `(missing rescue fallback)(41)`, from a collection, or from the value of a `begin` expression, as in `(begin JSON::parse end)("[8]")`. Target selection finishes before positional arguments, keywords, splats and an attached block are passed to the callee, so the rescue catches lookup failures while selecting the target and not errors from its arguments or body. Selected class methods and bound helpers retain their receivers while arguments run, targets live in accounted argument storage rather than escaping as values, and unwinding discards abandoned targets and arguments. Lookup errors are catchable at the expression where lookup fails. The [probe cases](computed-call-gaps.json) record the intentional differences from Go and remain part of the `compatibility` golden corpus.

Explicit calls to a member named `call` also resolve their target before evaluating arguments. See [member call selection](call-member.md).
