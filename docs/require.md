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

On `wasm32-wasip1`, configure roots with the guest paths of directories exposed by the host. Handles preserve root identity through path replacement and overlapping preopens. See [platform support](platforms.md) for host restrictions and verification.

Strict effects is disabled by default. Enable it with `Engine::set_strict_effects(true)` to require each invocation to set `CallOptions::allow_require`. Compiled scripts and their clones retain their engine mode; every call supplies its own permission, including concurrent calls and calls through the Tokio runner. The receiving script's mode and call permission also govern `require` inside imported functions, methods and host-returned modules. Permission applies to cached files as well as new loads, and operates within the configured roots, allow/deny rules and resource limits.

Argument expressions run before the permission check. A denied `require` raises a catchable `RuntimeError` beginning with `strict effects: ` before validating the builtin's signature, inspecting a file, compiling, initializing or populating the cache. Cancellation and exhausted budgets still terminate execution. The permission governs the builtin `require`; registered host callbacks and ordinary script functions remain explicitly available through their normal bindings. Rust's separate host-global and namespaced capability contracts remain unfinished.

`require(path: string, *, as: string? = nil)` takes the module name, and optionally an alias, as string literals (V0309), so the compiler resolves the file and checks it before the requiring script runs; the file's exports have the types their declarations give them. It returns an object containing the file's public top-level functions and enums. Ordinary `def` and `export def` are public; `private def`, classes and file variables stay private. Export names are also made available in the receiving execution root when they do not overwrite an existing binding. An alias must be an identifier and must not conflict with the root or current scope. Requiring the same file with the same alias is allowed. The static checker does not yet see a module's enums from the requiring script, so a function that returns one is typed `any` there.

Invalid arguments and aliases raise `RuntimeError`. Rust hosts can still identify these failures by `ErrorKind::Argument`.

```vibe module=counter.vibe
export def add(amount: int) -> int
  amount + 1
end
```

```vibe
counter = require("counter", as: "Counter")
counter.add(2) # 3
Counter.add(3) # 4
add(4)         # 5
```

Class and namespace initializers run before the file body. Successful initialization publishes exports and aliases; failed initialization can be rescued and retried. A file initializes once per call, while a later call starts independent state. Circular imports report their dependency chain.

Unreachable private state from failed initializations is reclaimed within the call, including instance data and unused class metadata. Rejected aliases do not execute the file body. Retrying a failed parent preserves dependencies that initialized successfully, and state explicitly retained by host callbacks remains valid. Pending calls and writes keep their targets alive during argument evaluation and collection.

Exported functions remain attached to their module: `counter.add(2)` calls one, and a function is never a value that can be stored, passed or returned. A zero-argument export is called without parentheses. A required file binds its own functions, and the receiving root binds published exports. This follows the [documented callable restriction](compatibility.md#builtin-descriptors).

Returned module objects retain their compiled code, host callbacks and private environment. Importing them into any script call copies their mutable state, preserving shared references within that call. Execution uses the receiving limits, cancellation token and module policy. Required code can resolve receiving root functions, host functions, nominal declarations and published aliases; the receiver's ordinary function locals remain private. Assigning a name inside the required file creates or updates its own binding. The file's top-level locals, functions and declarations share one scope, so a call such as `helper = helper(1)` assigning a file-scope name skips that whole scope and resolves the call in the receiving root, failing with `undefined variable helper` when nothing there answers. The same applies inside the file's functions and blocks, while a parameter or block parameter of that name is skipped alone.

Production mode reuses cached compilation until `Engine::clear_module_cache`. Development mode rechecks file metadata between calls. Active calls pin both normalized requests and resolved files, so cache clearing or file replacement does not change their selected code. Changing the module configuration or registered host callbacks creates a new loader snapshot for subsequent scripts; previously compiled scripts keep their earlier configuration and callbacks.

Syntax and execution diagnostics identify required source files by their root-relative filename. Each call frame identifies the file containing that frame's position, including calls between required files and unnamed host scripts. Rescued errors preserve the same named snippets and backtraces. Diagnostic filename storage does not retain the file's compiled code, callbacks or filesystem root; see [source diagnostics](diagnostics.md).

A required file's syntax failure is a catchable `RuntimeError`. Hosts still see `ErrorKind::Syntax`, the original filename and the parse position. Rescue and ensure run normally, failed compilation is not cached, and a later call can retry a corrected file. Host-side `Engine::compile` syntax failures have no script exception class. Cancellation, deadlines and exhausted execution budgets remain uncatchable.

Source reads, module state, exported descriptors, pending calls and imported environments use the receiving work and memory budgets. Exhaustion and cancellation remain uncatchable.

Cold compilation also uses the receiving work budget, cancellation token and deadline while lexing, resolving ambiguous tokens, parsing, walking declarations and types, copying aliased method bodies, generating bytecode and building source diagnostics. Speculative parsing preserves termination errors. A stopped compile neither initializes the module nor publishes it to the cache. Cached code avoids those compilation charges; reloading changed files or clearing the cache requires a new compilation.

Compiled code remains outside the invocation memory quota. Cold compilation charges token-buffer capacity, including nested interpolation tokens, copied token streams and both sides of the parser's editable token buffer. Growth reserves overlapping old and new storage, and consuming iterators retain their charge until their backing allocation is released. Memory exhaustion stays latched through speculative parsing. Owned literal bytes, numeric text, interpolation and percent-word containers, and boxed token data are also charged before allocation. Identifier tokens borrow the original source. Literal payloads keep their charges through syntax-tree and alias lifetimes; cached constants share the data without keeping the compiling call's budget alive. Syntax-tree boxes and expression, statement, argument, parameter, declaration and rescue containers remain charged through generation, including moved declarations and copied method aliases. Alias copies are fallible and release partially copied trees on failure. Owned syntax and generated names, type field bytes and type containers also remain charged while live. Aliases share immutable names and reserve separate type containers; compiled type metadata does not retain the originating invocation budget. Parser tables charge local bindings, enum duplicate checks, visibility directives and copied scopes. Growth and deleted-slot compaction reserve old and new storage together; failed operations preserve existing bindings. Name hashing and comparisons check work and cancellation every 4 KiB. Bytecode generation also charges local, parameter, read and assignment tables, copied outer scopes, declaration contexts, loop bindings, temporary binding-name lists, case jump lists, normalized enum-symbol checks and type-label builders. Scope copies share immutable names while reserving their own table storage. Final compiled function and handler metadata release the originating invocation charges. Compiler error messages, diagnostic headers, retained filename bytes and source snippets reserve capacity before construction. Counted and written diagnostic text checks work and cancellation in bounded chunks; failed formatting preserves latched exhaustion and releases partial storage. Rescue handling takes over the storage charge without charging a duplicate allocation. Cold loading borrows host registrations instead of copying a temporary registry. Builtin namespace builders create their final entries directly, without temporary duplicate-key maps or conversion lists. Invalid source encoding also uses the accounted compiler error builder. See the [compiler allocation audit](compiler-accounting.md) for ownership boundaries and verification. Source and cache bounds still apply. Host-side `Engine::compile` retains its source and syntax bounds without an invocation budget. Full module conformance, remaining host capability contracts and native Linux/Windows integration remain unfinished.
