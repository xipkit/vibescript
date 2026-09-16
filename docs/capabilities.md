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

Methods accept positional and keyword arguments. Direct, scoped, immediate indexed, safe-navigation and `send`/`public_send` calls share the same contracts. Object fields can override builtin method names. Argument validation runs before the callback, and return validation runs on every successful result after import into the receiving budget. Returning an error skips return validation because no result exists. Validators belong to the descriptor's identity, so identical diagnostic names cannot share or transfer contracts. Factories may return new objects containing independently validated methods.

Callbacks, factories and validators receive cancellation and deadlines through `CallContext`. The runtime checks the context before and after each host boundary. Ignored step or memory exhaustion remains latched; cancellation and exhaustion cannot be rescued or followed by script cleanup effects. Ordinary method errors retain their host `ErrorKind` and script exception class, with the calling script's diagnostics. Use `Error::with_class` when the adapter needs a specific rescue class. Binding failures occur before script execution.

Later capability grants replace earlier grants of the same name. Explicit call globals take precedence over capability bindings, including globals containing `nil`; shadowed factories still run. Parameters and lexical bindings retain their normal precedence. Strict-effects scripts validate all globals as data before any capability factory runs, while methods supplied through the explicit capability channel remain available. Required files use the receiving call's grants.

A host-owned `HostMethod::value()` is a reusable grant template. Its first import binds it to the receiving invocation. A saved script namespace or object graph retains that invocation's grant; importing it into a later call cannot reactivate the old method, even if the later call receives a fresh capability with the same name. Reach the new grant through its root binding instead. Concurrent calls have independent grants and limits.

The selected ADR-006 policy keeps capability methods attached to their bindings or namespaces. This restriction was explicitly selected on 2026-09-16. `SMS.send(...)` and `SMS[:send](...)` are calls; extracting, storing, passing or returning `SMS[:send]` as an executable value is rejected. Go v0.70.0 permits some extraction through indexed and scoped reads. Stateless core builtin descriptors keep their existing separate compatibility behavior.

Imported containers, descriptor names and metadata, binding storage, traversal work and callback results count against the invocation's limits. Descriptors deferred for safe callback destruction retain their metadata charge until destruction; repeated references share that reservation. Returned or host-retained values retain their own charges. Callback closure captures and allocations made independently by trusted host code remain host-owned; callbacks must cooperate with cancellation and account their work. Rust's immutable values isolate arrays and hashes across the host boundary.

Attached blocks are currently rejected before validation or host invocation. Host-driven block invocation and retirement, native async methods, published static signatures, and a live mutable capability-object publication API remain unfinished. Existing flat `Engine::register` callbacks and the optional Tokio runner remain available. This milestone does not complete the language port.
