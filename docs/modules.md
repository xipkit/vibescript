# Module declarations

Source modules group constants and methods:

```vibescript
module Scoring
  BONUS = 10

  def self.with_bonus(score)
    score + BONUS
  end
end

def run
  Scoring.with_bonus(80)
end
```

This returns `90`. Methods use the same positional, default, keyword, rest, type-annotation and synchronous-block contracts as other script functions. Modules can nest. Their names start with an ASCII uppercase letter.

`Scoring::BONUS` reads a constant. Dotted access checks methods before fields, so a method can share a constant's name. Scoped access never invokes a method. `def self.name=` declares a setter; assignments retain the assigned value even when the setter returns something else.

Use `public`, `private` or `protected` as a visibility section, before a method definition, or with a previously defined method name such as `private :helper`. Private methods require an implicit receiver. Protected methods allow an explicit receiver from the same module. `respond_to?` reports methods and respects visibility; fields are data.

Module bodies run once per invocation, with nested bodies initialized before their parent. Snippet bodies execute at their declaration's position. A body can read earlier top-level locals and update a lower-case local already bound there. Module methods have their own declaration scope. Assignments in a block stop at the module-body boundary; reads and addressed collection mutations can still reach ambient values.

`@@name` accesses a module variable. Uppercase assignments within a module create its constants. `M.name = value` writes a field or calls its setter. Nested indexed assignment through `M::ARRAY` updates that field; `M.ARRAY` is an evaluated getter result.

Module aliases, including `dup`, share identity and state within a call. Collections stored in fields retain value semantics: assigning an array to a field does not let later field mutations change the original local, and a returned array keeps its earlier value. These two selected differences from Go are recorded in [the compatibility audit](compatibility.md).

Module fields, namespace metadata, imported values, call frames and pending writes use the invocation's memory budget. Lookups, initialization and method execution consume steps and observe cancellation. State is released on completion or failure, including references from a module field back to the same module.

Escaped namespace and instance values keep their original compiled code and host callbacks alive after the originating script is dropped. Each call holds imported code through object cleanup, then releases those temporary references before reporting memory usage. Callback cleanup runs outside the object-heap lock, and cancellation is checked again before returning success.

This implements source namespace declarations; [classes](classes.md) are also supported. `require`/`load`, file exports and host capability namespaces remain pending. Escaped namespace values currently preserve declaration identity, and a subsequent call starts fresh state. Calling a namespace from a different compiled script is currently rejected. The selected [cross-script isolation contract](compatibility.md#state-isolation-across-host-calls) requires each receiving call to own independent mutable state; dispatch and state transfer still need implementation. General introspection coverage, including `is_type?`, remains part of the standard-library work.
