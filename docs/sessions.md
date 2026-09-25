# Interactive sessions

Three small APIs let a host keep state from one compiled snippet to the next, as `vibes repl` does. Each works on its own; none changes how an ordinary call runs.

## Builtin names

`vibescript::builtins()` returns the core builtins every script can reach by name, like Go's `Engine.Builtins`. Functions such as `puts` map to builtin descriptors, and namespaces such as `JSON` and `Math` map to objects whose fields are their members, including constants such as `Math::PI`:

```rust
let catalog = vibescript::builtins();
assert_eq!(catalog["puts"].type_name(), "builtin");
let json: Vec<_> = catalog["JSON"]
    .as_hash()
    .unwrap()
    .iter()
    .map(|(name, _)| String::from_utf8_lossy(name.as_bytes().unwrap()).into_owned())
    .collect();
assert_eq!(json, ["parse", "parse_as", "stringify"]);
```

Tools complete and list names from this map instead of keeping a parallel table. It describes the language, so registered host functions and capabilities are not included, and the removed `proc`, `lambda` and `Proc` constructors are absent. Scripts still cannot hold a descriptor as a value.

## Declarations

`Script::declarations()` lists the script's top-level `def`, `class`, `module` and `enum` declarations in source order, with the byte range of each in the compiled source. A span starts at the first keyword, including `private` or `export`, and ends with the final token, so `def a; 1; end; a` yields `def a; 1; end`. A top-level `alias` is listed as a function. Declarations nested in classes, modules or other code are not listed.

```rust
use vibescript::{DeclarationKind, Engine};

fn main() -> vibescript::Result<()> {
    let source = "x = 1\ndef double(n: int) -> int\n  n * 2\nend\ndouble(x)";
    let script = Engine::new().compile(source)?;
    let declaration = &script.declarations()[0];
    assert_eq!(declaration.kind, DeclarationKind::Function);
    assert_eq!(&source[declaration.span.clone()], "def double(n: int) -> int\n  n * 2\nend");
    Ok(())
}
```

Compiling the text of earlier function declarations ahead of new source carries them into a later script. Classes, modules and enums are better carried as the values `run_bindings` returns: recompiling their source creates new types, so instances made earlier would no longer match them. The list is compiled metadata; building it charges the compile's work budget like the rest of the parse.

## Root bindings

`Script::run_bindings(options)` runs the top-level statements like `Script::run` and also returns the root bindings they leave: every entry of `CallOptions::globals` with the value the run left in it, the classes, modules and enums the script declares at the top level, and every top-level local the statements assigned. A local takes precedence over a global of the same name, and a supplied global shadows a declaration of the same name, as it does during the run. A local that only an unexecuted branch assigns is bound to `nil`, as a later read in the same script would see it.

```rust
use std::collections::BTreeMap;
use vibescript::{CallOptions, Engine};

fn main() -> vibescript::Result<()> {
    let engine = Engine::new();
    let mut session = BTreeMap::new();
    for source in [
        "class Box\n  @n: int\n\n  def initialize(n: int)\n    @n = n\n  end\nend\nitems = [Box.new(1)]",
        "items.push(Box.new(2))\ncount = items.length",
        "kept = items.all? { |item| item.is_type?(:Box) }",
    ] {
        let options = CallOptions { globals: session, ..CallOptions::default() };
        session = engine.compile(source)?.run_bindings(options)?.1;
    }
    assert_eq!(session["count"].as_int(), Some(2));
    assert_eq!(session["kept"].truthy(), true);
    Ok(())
}
```

The values follow the result's contracts. They are isolated snapshots: the host's original globals are unchanged, and a later call cannot change a returned value. Passing them back as globals continues the session. Instances keep their fields and compiled code, and still belong to the class values passed with them, so `is_type?`, type annotations and enum comparisons behave as in one script. Each call starts class and module state afresh, as for any separately compiled namespace. A module object returned by `require` keeps its private file state, but the export names that `require` published into the root are not bindings; keep the returned object to use them later. Functions are not values, so they are not bindings; carry them as source with `Script::declarations`. Capturing the bindings charges the run's work budget, and a failed run returns only its error.
