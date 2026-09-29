<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-light.svg">
    <img src="docs/logo-dark.svg" alt="Vibescript" height="60">
  </picture>
</p>

# Vibescript

Vibecoding will have a profound impact on personal computing. Software can
become malleable: a solid base package that people shape around how they work,
instead of waiting for every customization to be built into the product.

Vibecoding still needs expertise. Generated code must be understood and
maintained, or it can rot and become vulnerable to attack. Vibescript helps
power this future: a small, statically typed scripting language for
customizations on top of an existing app. The host decides which capabilities
scripts can access and how much work they can do.

Vibescript’s Ruby-inspired syntax is easy to read. Its type checker catches
mistakes before execution and offers fixes in its diagnostics, so people and AI
can write custom behavior while the host app stays in control.

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
