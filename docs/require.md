# Required source files

Configure file loading before compiling a script:

```rust,no_run
use vibescript::{CallOptions, Engine, ModuleConfig};

let mut engine = Engine::new();
engine.set_strict_effects(true);
engine.set_module_config(ModuleConfig {
    paths: vec!["scripts".into()],
    ..ModuleConfig::default()
})?;
let script = engine.compile("require(\"counter\")")?;
script.run(CallOptions {
    allow_require: true,
    ..CallOptions::default()
})?;
# Ok::<(), vibescript::Error>(())
```

Roots are opened when configured. Non-relative requests search those roots in order. Relative requests such as `require("./helpers")` resolve from the executing required file's origin. The loader confines filesystem access through directory handles, validates filename spelling, applies allow/deny patterns and reads only bounded regular files. The default source limit is one MiB per file and the compilation cache holds at most 1,000 modules. Zero selects these defaults.

Strict effects is disabled by default. Enable it with `Engine::set_strict_effects(true)` to require each invocation to set `CallOptions::allow_require`. Compiled scripts and their clones retain their engine mode; every call supplies its own permission, including concurrent calls and calls through the Tokio runner. The receiving script's mode and call permission also govern `require` inside imported functions, methods and host-returned modules. Permission applies to cached files as well as new loads, and operates within the configured roots, allow/deny rules and resource limits.

Argument expressions run before the permission check. A denied `require` raises a catchable `RuntimeError` beginning with `strict effects: ` before validating the builtin's signature, inspecting a file, compiling, initializing or populating the cache. Cancellation and exhausted budgets still terminate execution. The permission governs the builtin `require`; registered host callbacks and ordinary script functions remain explicitly available through their normal bindings. Rust's separate host-global and namespaced capability contracts remain unfinished.

`require` takes one string or symbol, an optional `as:` alias and no block. It returns an object containing the file's public top-level functions and enums. Ordinary `def` and `export def` are public; `private def`, classes and file variables stay private. Export names are also made available in the receiving execution root when they do not overwrite an existing binding. An alias must be an identifier and must not conflict with the root or current scope. Requiring the same file with the same alias is allowed.

```vibescript
counter = require("counter", as: :Counter)
counter.add(2)
Counter.add(3)
```

Class and namespace initializers run before the file body. Successful initialization publishes exports and aliases; failed initialization can be rescued and retried. A file initializes once per call, while a later call starts independent state. Circular imports report their dependency chain.

Unreachable private state from failed initializations is reclaimed within the call, including instance data and unused class metadata. Rejected aliases do not execute the file body. Retrying a failed parent preserves dependencies that initialized successfully, and state explicitly retained by host callbacks remains valid. Pending calls and writes keep their targets alive during argument evaluation and collection.

Exported functions remain attached to their module. `counter.add(2)`, `counter::add(2)` and `counter[:add](2)` call them. Reading `counter[:add]` or `counter::add` as data raises a type error, as do storage, arguments, returns and collection operations that extract function values. A zero-parameter dotted member can auto-invoke. Existing stateless builtin descriptors retain their separate behavior. This follows the selected [documented callable restriction](compatibility.md#builtin-descriptors), including where Go v0.70.0 accepts detached functions.

Returned module objects retain their compiled code, host callbacks and private environment. Importing them into any script call copies their mutable state, preserving shared references within that call. Execution uses the receiving limits, cancellation token and module policy. Required code can resolve receiving root functions, host functions, nominal declarations and published aliases; the receiver's ordinary function locals remain private. Assigning a name inside the required file creates or updates its own binding.

Production mode reuses cached compilation until `Engine::clear_module_cache`. Development mode rechecks file metadata between calls. Active calls pin both normalized requests and resolved files, so cache clearing or file replacement does not change their selected code. Changing the module configuration or registered host callbacks creates a new loader snapshot for subsequent scripts; previously compiled scripts keep their earlier configuration and callbacks.

Syntax and execution diagnostics identify required source files by their root-relative filename. Each call frame identifies the file containing that frame's position, including calls between required files and unnamed host scripts. Rescued errors preserve the same named snippets and backtraces. Diagnostic filename storage does not retain the file's compiled code, callbacks or filesystem root; see [source diagnostics](diagnostics.md).

Source reads, module state, exported descriptors, pending calls and imported environments use the receiving work and memory budgets. Exhaustion and cancellation remain uncatchable. Cold compilation has source and cache bounds and checkpoints around compilation; the broader compiler accounting/cancellation audit, full module conformance and remaining host capability contracts remain part of the unfinished language port. Native Linux/Windows integration has not been verified for this change.
