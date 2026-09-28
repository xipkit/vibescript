# Module declarations

Source modules group constants and methods:

```vibe
module Scoring
  BONUS = 10

  def self.with_bonus(score: int) -> int
    score + BONUS
  end
end

def run -> int
  Scoring.with_bonus(80)
end
```

This returns `90`. Methods follow the same rules as other script functions: typed positional, default, rest and `*` keyword parameters, a declared result, and a typed `&block` parameter when they yield. Modules can nest, and a nested module or class is named `Outer::Inner`. Their names start with an ASCII uppercase letter. A module may also declare type aliases, `type Score = int`, used inside it and, qualified, outside.

`Scoring::BONUS` reads a constant. Dotted access checks methods before fields, so a method can share a constant's name. `def self.name=` declares a setter; assignments retain the assigned value even when the setter returns something else.

Use `public`, `private` or `protected` as a visibility section, before a method definition, or with a previously defined method name such as `private :helper`. Private methods require an implicit receiver. Protected methods allow an explicit receiver from the same module.

Module bodies run once per invocation, with nested bodies initialized before their parent. Snippet bodies execute at their declaration's position. A body can read earlier top-level locals and update a lower-case local already bound there. Module methods have their own declaration scope. Assignments in a block stop at the module-body boundary; reads and addressed collection mutations can still reach ambient values.

`@@name` accesses a module variable, declared with a value as `@@count: int = 0`. Uppercase assignments within a module create its constants, and `NAMES: array<string> = []` declares one's type. `M.name = value` writes a field or calls its setter. Nested indexed assignment through `M::ARRAY` updates that field; `M.ARRAY` is an evaluated getter result.

Module aliases, including `dup`, share identity and state within a call. Collections stored in fields retain value semantics: assigning an array to a field does not let later field mutations change the original local, and a returned array keeps its earlier value.

Module fields, namespace metadata, imported values, call frames and pending writes use the invocation's memory budget. Lookups, initialization and method execution consume steps and observe cancellation. State is released on completion or failure, including references from a module field back to the same module.

Escaped namespace and instance values keep their original compiled code and host callbacks alive after the originating script is dropped. Each call holds imported code through object cleanup, then releases those temporary references before reporting memory usage. Callback cleanup runs outside the object-heap lock, and cancellation is checked again before returning success.

Namespaces and instances can be passed to another compiled script or engine. Their methods, constructors, operators, properties and host callbacks use the original compiled code. Each receiving call creates independent class/module state and source-program globals; imported instance fields preserve their values, shared references and cycles within that call. Changes do not affect the source value or another call. Protected visibility compares the declaring class, so matching names or local declaration indices in separate programs grant no access.

A foreign source program initializes its class and module bodies when first imported into an invocation, before the receiving script continues. Incoming argument programs initialize before the entry function. Programs returned by host callbacks initialize on the same VM stack, so nested imports finish before their importing initializer resumes. Initialization consumes the receiving budget and observes its cancellation token. A receiving rescue can catch an initialization error from a host-produced value; later access to that failed program is rejected for the rest of the call. A new call starts fresh initialization.

Rejected host values and values that a callback imports but does not return keep their code alive for safe cleanup without running its initializers. A later successful return of the same source value initializes it normally.

Required files distinguish multiple captured environments of the same compiled program. Captured class/module state and completed initializer markers survive in returned values, and every receiving call copies that state. Aliases within a call share their imported scope; separate environments have distinct identity, protected access and nominal types. Initialization failures belong to one environment, not every use of its compiled code. Ordinary source declarations retain the fresh initialization behavior above.

This implements source namespace declarations and [classes](classes.md), including the selected [cross-script isolation contract](compatibility.md#state-isolation-across-host-calls). [File loading](require.md) adds `require`, private file state and attached exported calls. Host capability namespaces are described in [host capabilities](capabilities.md).
