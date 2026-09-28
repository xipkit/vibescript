# Host capabilities

`CallOptions.capabilities` grants host services to one invocation. A `Capability` factory receives that invocation's `CallContext` and returns a binding, usually an object containing `HostMethod` descriptors. The engine runs factories in order before script initializers and defaults. Each factory can create fresh callback state; cloning a capability shares the factory rather than its per-call state.

`Capability::from_value` grants an immutable binding template instead of a factory. Every invocation imports that same value, so its methods still receive the receiving call's fresh grant, while its data and published signatures can be read by the static checker without executing host code. Factories stay opaque to checking because inspecting their binding would require running them. Use a declared `Capability::from_value` template with published method signatures for statically callable services. Declaring a factory gives its name type `any`, which does not permit member calls; a factory is not a substitute for publishing a typed service contract.

A capability template cannot hold a function a script exports, at its root or at any depth, and neither a factory's value nor a global can be one: a script calls such a function only through its module, so declaring that template, binding that factory value or supplying that global fails with `ErrorKind::Argument`. Register a host function instead.

`Engine::declare_capability(&capability)` declares a capability that every call grants, typed by its template: host methods by their published signatures, or `any` arguments and result without one, and data by the types its values show. A factory declares its name as `any`. The static checker types the name as a namespace of those members, `Engine::prelude` lists it, and each call must grant a capability, or supply a global, of the name whose value has the declared members, with the same signatures and data types, before any script code runs. An object holding a host method at any depth is declared, and checked, as a namespace of its members, so a grant must keep each of its methods a host method. Namespaces nest 64 levels deep; an object nested deeper declares as `any`, as data nested deeper than its type is read does, so a template as deep as a value may be still declares in time linear in its size. With static types, a capability the host does not declare is an undefined name (V0201).

```rust
use vibescript::{CallOptions, Capability, Engine, HostMethod, Signature, SignatureParam, Value};

let send = HostMethod::new("SMS.send", |ctx, _, _| ctx.bytes(b"queued"))
    .with_signature(Signature {
        params: vec![SignatureParam { name: "message".into(), ty: "string".into(), optional: false }],
        result: "string".into(),
        accepts_block: false,
    })?;
let sms = Capability::from_value("SMS", Value::object(vec![(b"send".to_vec(), send.value())]));
let mut engine = Engine::new();
engine.set_strict_effects(true);
engine.declare_capability(&sms)?;
let script = engine.compile("def run -> string\n  SMS.send(\"hello\")\nend")?;
let result = script.call("run", &[], CallOptions {
    capabilities: vec![sms],
    ..CallOptions::default()
})?;
assert_eq!(result.value.as_bytes(), Some(b"queued".as_slice()));
// The published signature takes a string, so this does not compile.
assert!(engine.compile("def run -> string\n  SMS.send(1)\nend").is_err());
# Ok::<(), vibescript::Error>(())
```

Methods accept positional and keyword arguments. Direct and safe-navigation calls share the same contracts. Object fields can override builtin method names. Argument validation runs before the callback, and return validation runs on every successful result after import into the receiving budget. Returning an error skips return validation because no result exists. Validators belong to the descriptor's identity, so identical diagnostic names cannot share or transfer contracts. Factories may return new objects containing independently validated methods.

Callbacks, factories and validators receive cancellation and deadlines through `CallContext`. The runtime checks the context before and after each host boundary. Ignored step or memory exhaustion remains latched; cancellation and exhaustion cannot be rescued or followed by script cleanup effects. Ordinary method errors retain their host `ErrorKind` and script exception class, with the calling script's diagnostics. Use `Error::with_class` when the adapter needs a specific rescue class. Binding failures occur before script execution.

Later capability grants replace earlier grants of the same name. Explicit call globals take precedence over capability bindings, including globals containing `nil`; shadowed factories still run. Parameters and lexical bindings retain their normal precedence. Strict-effects scripts validate all globals as data before any capability factory runs, while methods supplied through the explicit capability channel remain available. Required files use the receiving call's grants.

A host-owned `HostMethod::value()` is a reusable grant template. Its first import binds it to the receiving invocation. A saved script namespace or object graph retains that invocation's grant; importing it into a later call cannot reactivate the old method, even if the later call receives a fresh capability with the same name. Reach the new grant through its root binding instead. A template supplied through `Capability::from_value` follows the same rule: a value saved from an earlier invocation keeps its expired grant, and the checker reports calls through it. Concurrent calls have independent grants and limits.

The ADR-006 policy keeps capability methods attached to their bindings or namespaces. `SMS.send(...)` is a call; a method is never a value that can be extracted, stored, passed or returned.

Imported containers, descriptor names and metadata, binding storage, traversal work and callback results count against the invocation's limits. Descriptors deferred for safe callback destruction retain their metadata charge until destruction; repeated references share that reservation. Returned or host-retained values retain their own charges. Callback closure captures and allocations made independently by trusted host code remain host-owned; callbacks must cooperate with cancellation and account their work. Rust's immutable values isolate arrays and hashes across the host boundary.

Use `HostMethod::new_with_block` for a synchronous block driver. Its callback receives a scoped `HostCall` with `block_given()`, `call_block(args)` and `context()`. The handle borrows the active invocation and cannot escape the callback or move to another thread; this enforces retirement without an executable script value. Repeated calls within the callback are allowed. `HostMethod::new` rejects attached blocks unless its published signature explicitly permits them; it does not expose a block handle.

```rust
use vibescript::{CallOptions, Engine, HostMethod};

let visit = HostMethod::new_with_block("visit", |call, args, _| {
    call.call_block(args)
});
let mut engine = Engine::new();
engine.register_method("visit", visit);
let script = engine.compile("visit(20) { |n| n.as(int) + 1 }")?;
let result = script.run(CallOptions::default())?;
assert_eq!(result.value.as_int(), Some(21));
# Ok::<(), vibescript::Error>(())
```

Without a published block parameter type, a yielded value has type `any`; the example narrows it with `.as(int)`. A registered host function is available to every script compiled by that engine; a declared capability must also be granted by each call.

`with_block_contract` also gives the argument validator a block-presence flag. Missing blocks are permitted unless the contract or callback requires one. Calling a missing block raises `RuntimeError` with `block required`. Yielded values are imported into the receiving budget and keep their source program and type information. Block parameters, captured variables, repeated calls and returned values follow ordinary value semantics. Values and ordinary errors retained by the callback keep their accounting reservations; dropping them releases those reservations.

A block's `next` returns to the driver. Its `break` terminates the receiving call and sends the break value through the host return contract. A nonlocal `return` validates at the defining script method. Inner rescue and ensure handlers run before ordinary failures reach the host; outer script handlers wait until the callback returns. The host may handle ordinary block errors and invoke the block again. Cancellation and exhausted step or memory quotas remain latched and prohibit later script effects, including rescue and ensure.

A pending `break` or `return` survives even if a host callback ignores `ErrorKind::ControlFlow`. Further block calls cannot execute script after that transfer.

## Publishing into the receiver

A granted capability object, like a call global holding host methods, is the host's live state for the duration of one invocation. A block-capable method publishes into it with `HostCall::set_receiver_field(key, value)`; async methods use the same method on `AsyncHostCall`. The write lands immediately in the binding that holds the receiver, whether that is the capability object itself or a hash nested inside it, exactly as if the script had assigned `cap[key] = value`. Later script reads, blocks the method runs and later host calls all observe it. The first publication in a host call finds the shallowest binding path holding the receiver; later publications in the same call write to that path, keeping script writes made there in between. Publication ends with the invocation; the next call binds a fresh capability.

```rust
use vibescript::{CallOptions, Capability, Engine, HostMethod, Value};

let install = HostMethod::new_with_block("config.install", |call, _, _| {
    call.set_receiver_field(b"limit", &Value::int(10))?;
    call.call_block(&[])
});
let config = Capability::from_value("config", Value::object(vec![
    (b"install".to_vec(), install.value()),
    (b"limit".to_vec(), Value::int(0)),
]));
let mut engine = Engine::new();
engine.declare_capability(&config)?;
let script = engine.compile("config.install { config.limit }")?;
let result = script.run(CallOptions {
    capabilities: vec![config],
    ..CallOptions::default()
})?;
assert_eq!(result.value.as_int(), Some(10));
# Ok::<(), vibescript::Error>(())
```

Values the script extracted earlier, such as `data = config["data"]` or a copy `c = config`, are independent values and do not change. A method called through an unmodified copy still publishes to the capability binding, and the copy itself stays as it was. A receiver that no capability binding holds, such as a copy the script has since changed, only changes for that method's later `receiver()` reads, and `set_receiver_field` returns `false`. Published values are imported into the invocation's accounting and must be data or `HostMethod` descriptors; a published descriptor receives the invocation's grant. Publication cannot replace a field that holds a method, and plain `HostMethod::new` callbacks, which have no receiver handle, cannot publish.

A declared capability's type comes from its template, so a field a method publishes later must already be in the template for scripts to read it with static types.

## Published signatures

`HostMethod::with_signature` declares positional parameters with `SignatureParam { name, ty, optional }`, a result type, and whether a block is accepted. Type strings use the script annotation grammar, including unions, nullable values, typed containers, shapes, enums and classes. Empty strings leave slots unconstrained. Malformed types and required parameters after optional ones fail when the descriptor is created. `signature()` exposes immutable metadata for host tooling, and the static checker types calls by it.

The runtime still validates arity, keyword rejection, block presence and parameter types before entering the callback, since host arguments can come from code compiled without the signature. Missing optional arguments stay omitted. Normalization follows script type rules, including symbols becoming enum members inside containers, without mutating the original argument. Custom argument validators see the original values; the callback receives normalized values. Imported results pass signature normalization before custom return validation. A block `break` is a method result and must satisfy its declared type, which the static checker also requires of its value (V0101); a nonlocal `return` belongs to its defining script function.

Named types resolve in the active source, including required-file defaults and file aliases, with the call root as fallback. A same-named root type cannot replace the file's own declaration. Signature metadata, normalization, retained values and error diagnostics stay subject to invocation accounting and cancellation. Methods retain the same attached-call restriction and per-call grant lifetime.

Use `Engine::register_method(name, method)` to register a descriptor, including its signature, validators and optional block driver, for subsequently compiled scripts. Earlier scripts keep their registration snapshot. Descriptors can also be supplied through `Capability` or ordinary call globals; strict effects still require the explicit capability channel for executable globals.

## Async methods

With the `tokio` feature, `HostMethod::new_async` accepts a callback returning `asynchronous::HostFuture`. Its scoped `AsyncHostCall` and arguments may be borrowed across awaits. Use `context()?` for accounting and cancellation, `block_given()` to inspect block presence, `receiver()` and `set_receiver_field(key, value)` to read and publish into the member receiver, and `call_block(Vec<Value>).await` to invoke the attached block with isolated arguments. Rust prevents overlapping block calls and handles that outlive the callback. Methods keep their ordinary signatures, validators, per-call grants and attachment restrictions. Static checking reads their published contracts without constructing or polling host futures.

```rust
use vibescript::{CallOptions, Engine, HostMethod, asynchronous::Runner};

let mut engine = Engine::new();
engine.register_method("visit", HostMethod::new_async("visit", |call, args, _| {
    Box::pin(async move {
        tokio::task::yield_now().await;
        call.context()?.charge(1)?;
        call.call_block(args.to_vec()).await
    })
}));
let script = engine.compile("def run -> any\n  visit(20) { |n| n.as(int) + 1 }\nend")?;
let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
let result = runtime.block_on(async {
    Runner::new(1)?.call(script, "run".into(), vec![], CallOptions::default()).await
})?;
assert_eq!(result.value.as_int(), Some(21));
# Ok::<(), vibescript::Error>(())
```

`Runner` executes script and synchronous host work on bounded blocking workers. A native async wait releases its worker so another invocation can run. A synchronous host callback that invokes an async block still occupies its existing worker and keeps its reservation until it returns. Nested script work uses that same thread, including with a single-thread blocking pool. No additional Tokio runtime is created. Calling an async method through `Script::call` produces a catchable host error requiring `Runner`.

Cancellation, deadlines and latched quota failures interrupt pending host futures. Host code must keep each future poll bounded and cooperate during synchronous work. A block's `break` and nonlocal `return` remain pending if the host ignores their control-flow error. Async or worker panics unwind the invocation and become host errors at the runner boundary. Dropping an invocation releases unreachable cycles without executing script cleanup effects; retained values stay valid and charged.

Dropping an unpolled block future has no effect. Dropping a polled block future cancels and retires the invocation, even if the host returns a successful value afterward. `context()` reports an error while its state remains on a retiring worker, and the engine recovers that worker before finishing the callback. Engine-owned suspended state, block arguments and bridge storage count against memory limits. Arbitrary allocations and captures made by trusted host code remain host-owned.
