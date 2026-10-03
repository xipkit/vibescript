# Host globals

`CallOptions.globals` supplies named root bindings for one invocation. Each call imports a value on first use and caches it for subsequent reads and writes. Unused values and values overwritten before their first read are not copied. Materialized values, binding storage and import work count against the receiving call's limits.

```rust
use vibescript::{CallOptions, Engine, Value};

let mut engine = Engine::new();
engine.declare_global("settings", "{ items: array<int> }")?;
let script = engine.compile("settings[\"items\"].push(2)\nsettings")?;
let settings = Value::hash(vec![(
    b"items".to_vec(),
    Value::array(vec![Value::int(1)]),
)]);
let mut options = CallOptions::default();
options.globals.insert("settings".into(), settings.clone());
let result = script.run(options)?;
# Ok::<(), vibescript::Error>(())
```

The returned `settings["items"]` is `[1, 2]`; the host's original remains `[1]`. Repeated and concurrent calls receive independent mutable state. Arrays and hashes preserve value semantics even when multiple globals or arguments share a source value. Instance references preserve aliases and cycles within the receiving call while isolating the original object graph.

Globals can shadow script functions, classes, enums, registered hosts and builtins. A bound `nil` still shadows the original name. Parameters, module constants, enclosing initializer locals and explicitly scoped block locals retain their own bindings. A block assignment updates an existing host binding when no nearer binding shadows it. Calls select their targets before arguments run; explicit named calls retain declared script-method dispatch when a constant shares that name. These rules deliberately avoid [Go's binding inconsistencies](compatibility.md#host-binding-precedence). Required files can read receiving globals and mutate nested values; file assignments keep their private binding boundary. Required-module aliases retain their existing conflict rules and cannot replace statically owned foreign functions.

Incoming enums rebind to declarations from the same compiled script, including values first read inside a nested container. Named type lookup materializes matching global bindings without importing unrelated values. Foreign classes and instances keep their compiled code and original host callbacks, with fresh invocation state and receiving execution limits. An unread foreign namespace does not run its initializer.

`Engine::declare_global(name, ty)` declares a global that every call supplies, with its type as an annotation; an empty type is `any`. The static checker types the name by its declaration, and a bare name that nothing in scope and no declaration explains is a compile error (`V0201`), so a script reads only declared globals. Each call of a subsequently compiled script must supply the global, or a capability of the name, and a global's value must have the declared type; otherwise the call fails before any script code runs, as an argument of the wrong type would. Assigning the name, at the top level or in a function or block, writes that binding rather than declaring a local, so the value must have the declared type (V0101), a typed declaration of the name must declare that type (V0102), and a read keeps the declared type, which no test narrows; a parameter or block parameter of the name is its own local. A required file's assignment of the name binds the file's own variable instead, which shadows the global in the file and takes the type of its value. `Engine::prelude` lists declared names by their declarations.

With `Engine::set_strict_effects(true)`, every global is validated before initializers, default arguments or script callbacks execute, including unused globals. Scalars, enums, regexes and collections of data are allowed. Functions, builtin descriptors, classes, instances, match-offset methods and type literals are rejected even when nested. Validation charges traversal work and temporary storage, respects cancellation, deadlines and the value-depth bound, and avoids repeatedly traversing shared subgraphs. Registered host capabilities remain available separately.

The host owns the original values and map. Their existing allocations are outside the invocation's allocation counter until values are imported; temporary validation allocations are charged. [Host capabilities](capabilities.md) provide per-call method grants and argument/return contracts through a separate channel. Explicit globals take precedence when their names collide with capability bindings. Native async host callbacks are described in [async methods](capabilities.md#async-methods).
