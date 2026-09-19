# Host capabilities

`CallOptions.capabilities` grants host services to one invocation. A `Capability` factory receives that invocation's `CallContext` and returns a binding, usually an object containing `HostMethod` descriptors. The engine runs factories in order before script initializers and defaults. Each factory can create fresh callback state; cloning a capability shares the factory rather than its per-call state.

```rust
use vibescript::{CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Value};

let sms = Capability::new("SMS", |_| {
    let send = HostMethod::new("SMS.send", |ctx, args, _| {
        ctx.charge(1)?;
        // A real adapter calls its SMS service here and returns the receipt.
        ctx.bytes(args[0].as_bytes().unwrap())
    }).with_contract(
        |_, args, keywords| {
            if args.len() != 1 || args[0].as_bytes().is_none() || !keywords.is_empty() {
                return Err(Error::new(ErrorKind::Argument, "SMS.send expects a message string"));
            }
            Ok(())
        },
        |_, result| {
            if result.as_bytes().is_none() {
                return Err(Error::new(ErrorKind::Type, "SMS.send must return a receipt string"));
            }
            Ok(())
        },
    );
    Ok(Value::object(vec![(b"send".to_vec(), send.value())]))
});

let mut engine = Engine::new();
engine.set_strict_effects(true);
let script = engine.compile("SMS.send(\"hello\")")?;
let result = script.run(CallOptions {
    capabilities: vec![sms],
    ..CallOptions::default()
})?;
# Ok::<(), vibescript::Error>(())
```

`Capability::from_value` grants an immutable binding template instead of a factory. Every invocation imports that same value, so its methods still receive the receiving call's fresh grant, while its data and published signatures can be read by the static checker without executing host code. Factories stay opaque to checking because inspecting their binding would require running them; keep `Capability::new` for callbacks that need fresh per-call state.

```rust
use vibescript::{CallOptions, Capability, Engine, HostMethod, Signature, SignatureParam, Value};

let send = HostMethod::new("SMS.send", |ctx, _, _| ctx.bytes(b"queued"))
    .with_signature(Signature {
        params: vec![SignatureParam { name: "message".into(), ty: "string".into(), optional: false }],
        result: "string".into(),
        accepts_block: false,
    })?;
let options = CallOptions {
    capabilities: vec![Capability::from_value(
        "SMS",
        Value::object(vec![(b"send".to_vec(), send.value())]),
    )],
    ..CallOptions::default()
};
let mut engine = Engine::new();
engine.set_strict_effects(true);
let script = engine.compile("def run -> string; SMS.send(\"hello\"); end")?;
assert!(script.check_call("run", &[], &options)?.is_clean());
let bad = engine.compile("def run; SMS.send(1); end")?;
assert!(!bad.check_call("run", &[], &options)?.is_clean());
# Ok::<(), vibescript::Error>(())
```

Methods accept positional and keyword arguments. Direct, scoped, immediate indexed, safe-navigation and `send`/`public_send` calls share the same contracts. Object fields can override builtin method names. Argument validation runs before the callback, and return validation runs on every successful result after import into the receiving budget. Returning an error skips return validation because no result exists. Validators belong to the descriptor's identity, so identical diagnostic names cannot share or transfer contracts. Factories may return new objects containing independently validated methods.

Callbacks, factories and validators receive cancellation and deadlines through `CallContext`. The runtime checks the context before and after each host boundary. Ignored step or memory exhaustion remains latched; cancellation and exhaustion cannot be rescued or followed by script cleanup effects. Ordinary method errors retain their host `ErrorKind` and script exception class, with the calling script's diagnostics. Use `Error::with_class` when the adapter needs a specific rescue class. Binding failures occur before script execution.

Later capability grants replace earlier grants of the same name. Explicit call globals take precedence over capability bindings, including globals containing `nil`; shadowed factories still run. Parameters and lexical bindings retain their normal precedence. Strict-effects scripts validate all globals as data before any capability factory runs, while methods supplied through the explicit capability channel remain available. Required files use the receiving call's grants.

A host-owned `HostMethod::value()` is a reusable grant template. Its first import binds it to the receiving invocation. A saved script namespace or object graph retains that invocation's grant; importing it into a later call cannot reactivate the old method, even if the later call receives a fresh capability with the same name. Reach the new grant through its root binding instead. A template supplied through `Capability::from_value` follows the same rule: a value saved from an earlier invocation keeps its expired grant, and the checker reports calls through it. Concurrent calls have independent grants and limits.

The selected ADR-006 policy keeps capability methods attached to their bindings or namespaces. This restriction was explicitly selected on 2026-09-16. `SMS.send(...)` and `SMS[:send](...)` are calls; extracting, storing, passing or returning `SMS[:send]` as an executable value is rejected. Go v0.70.0 permits some extraction through indexed and scoped reads. Stateless core builtin descriptors keep their existing separate compatibility behavior.

Imported containers, descriptor names and metadata, binding storage, traversal work and callback results count against the invocation's limits. Descriptors deferred for safe callback destruction retain their metadata charge until destruction; repeated references share that reservation. Returned or host-retained values retain their own charges. Callback closure captures and allocations made independently by trusted host code remain host-owned; callbacks must cooperate with cancellation and account their work. Rust's immutable values isolate arrays and hashes across the host boundary.

Use `HostMethod::new_with_block` for a synchronous block driver. Its callback receives a scoped `HostCall` with `block_given()`, `call_block(args)` and `context()`. The handle borrows the active invocation and cannot escape the callback or move to another thread; this enforces retirement without an executable script value. Repeated calls within the callback are allowed. `HostMethod::new` rejects attached blocks unless its published signature explicitly permits them; it does not expose a block handle.

```rust
use vibescript::{CallOptions, Capability, Engine, HostMethod};

let visit = HostMethod::new_with_block("visit", |call, args, _| {
    call.call_block(args)
});
let script = Engine::new().compile("visit(20) { |n| n+1 }")?;
let result = script.run(CallOptions {
    capabilities: vec![Capability::new("visit", move |_| Ok(visit.value()))],
    ..CallOptions::default()
})?;
assert_eq!(result.value.as_int(), Some(21));
# Ok::<(), vibescript::Error>(())
```

`with_block_contract` also gives the argument validator a block-presence flag. Missing blocks are permitted unless the contract or callback requires one. Calling a missing block raises `RuntimeError` with `block required`. Yielded values are imported into the receiving budget and keep their source program and type information. Block parameters, captured variables, repeated calls and returned values follow ordinary value semantics. Values and ordinary errors retained by the callback keep their accounting reservations; dropping them releases those reservations.

A block's `next` returns to the driver. Its `break` terminates the receiving call and sends the break value through the host return contract. A nonlocal `return` validates at the defining script method. Inner rescue and ensure handlers run before ordinary failures reach the host; outer script handlers wait until the callback returns. The host may handle ordinary block errors and invoke the block again. Cancellation and exhausted step or memory quotas remain latched and prohibit later script effects, including rescue and ensure.

The explicitly selected control-flow policy preserves a pending `break` or `return` even if a host callback ignores `ErrorKind::ControlFlow`. Further block calls cannot execute script after that transfer. Go v0.70.0 permits swallowing these signals and running the block again. This behavior is explicitly selected and recorded separately in the compatibility audit.

## Published signatures

`HostMethod::with_signature` declares positional parameters with `SignatureParam { name, ty, optional }`, a result type, and whether a block is accepted. Type strings use the script annotation grammar, including unions, nullable values, typed containers, shapes, enums and classes. Empty strings leave slots unconstrained. Malformed types and required parameters after optional ones fail when the descriptor is created. `signature()` exposes immutable metadata for host tooling and the gradual script checker.

The runtime validates arity, keyword rejection, block presence and parameter types before entering the callback. Missing optional arguments stay omitted. Normalization follows script type rules, including symbols becoming enum members inside containers, without mutating the original argument. Custom argument validators see the original values; the callback receives normalized values. Imported results pass signature normalization before custom return validation. A block `break` is a method result and must satisfy its declared type; a nonlocal `return` belongs to its defining script function.

Named types resolve in the active source, including required-file defaults and file aliases, with the call root as fallback. A same-named root type cannot replace the file's own declaration. Signature metadata, normalization, retained values and error diagnostics stay subject to invocation accounting and cancellation. Methods retain the same attached-call restriction and per-call grant lifetime.

Use `Engine::register_method(name, method)` to register a descriptor, including its signature, validators and optional block driver, for subsequently compiled scripts. Earlier scripts keep their registration snapshot. Descriptors can also be supplied through `Capability` or ordinary call globals; strict effects still require the explicit capability channel for executable globals.

## Async methods

With the `tokio` feature, `HostMethod::new_async` accepts a callback returning `asynchronous::HostFuture`. Its scoped `AsyncHostCall` and arguments may be borrowed across awaits. Use `context()?` for accounting and cancellation, `block_given()` to inspect block presence, and `call_block(Vec<Value>).await` to invoke the attached block with isolated arguments. Rust prevents overlapping block calls and handles that outlive the callback. Methods keep their ordinary signatures, validators, per-call grants and attachment restrictions. Static checking reads their published contracts without constructing or polling host futures.

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
let script = engine.compile("def run;visit(20){|n|n+1};end")?;
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

Remaining static analysis and full language/platform conformance work are tracked in [the language port](language-port.md). This milestone does not complete the port.
