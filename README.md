<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-light.svg">
    <img src="docs/logo-dark.svg" alt="Vibescript" height="60">
  </picture>
</p>

# Vibescript

Apps often need to let people write their own workflows without giving them a
blank canvas or the keys to the whole system. As AI makes custom code easier to
generate, products can offer a small set of useful, predictable building blocks
instead. Think HyperCard: flexible, but within bounds.

Vibescript is a small, statically typed language for those workflows. It’s
Ruby-inspired and easy to read, and the host app decides what scripts can access
and how much work they can do. The Rust runtime checks types before a script runs
and reports errors with fixes, which makes the language practical for people and
AI to write.

## The language

Here’s a Vibescript function that totals a list of line items. Types are part of
the function signature, and blocks make collection work straightforward:

```vibe
def total(items: array<{ price: int, qty: int }>) -> int
  items.sum { |item| item["price"] * item["qty"] }
end

total([{ price: 250, qty: 2 }, { price: 100, qty: 1 }]) # 600
```

The [language guide](docs/language.md) walks through the whole language with
examples. Run `vibes prelude` to see the built-in functions and their signatures.

## Try it

From a checkout, run the example or open the REPL:

```sh
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]'
./scripts/cargo run --release -p vibes -- repl
```

The [CLI guide](docs/cli.md) covers the commands for running, checking,
formatting and testing scripts, plus editor setup.

## Use it in a Rust app

Compile a script and call a function from your app:

```rust
use vibescript::{CallOptions, Engine, Value};

let mut engine = Engine::new();
let script = engine.compile("def run(n: int) -> int\n  n + 1\nend")?;
let result = script.call("run", &[Value::int(41)], CallOptions::default())?;

assert_eq!(result.value.as_int(), Some(42));
```

You can expose typed host functions and capabilities, then set the limits for
each run. The [embedding docs](docs/capabilities.md) show how host code and
scripts fit together.

## A few more places to look

- [Tested workflow examples](corpus/glue/README.md)
- [AI authoring study](docs/authoring-evaluation.md)
- [Editor and language server setup](docs/lsp.md)
- [WASI and platform support](docs/platforms.md)
- [Runtime and VM design](docs/vm.md)

The CLI, REPL, formatter, test runner and language server are all part of the
project. The [CLI guide](docs/cli.md) has the details.
