# Editor tooling views

`vibescript::tooling` gives editor integrations, such as the `vibes lsp` language server, read-only views of source declarations and of the language's reserved words and member tables. Nothing here compiles, checks or runs code.

```rust
use vibescript::tooling::{ItemKind, member_receiver, outline};

let source = "class Wallet\n  def balance(currency: string) -> int\n    total = 0\n    total\n  end\nend\n";
let outline = outline(source)?;
let wallet = &outline.items[0];
assert_eq!((wallet.kind, wallet.name.as_str()), (ItemKind::Class, "Wallet"));
let balance = &wallet.children[0];
assert_eq!((balance.kind, balance.position.line), (ItemKind::Method, 2));
let function = balance.function.as_ref().unwrap();
assert_eq!(function.params[0].type_annotation.as_deref(), Some("string"));
assert_eq!(function.locals, ["total"]);

let probe = "def f(s: string)\n  s.probe\nend\n";
assert_eq!(member_receiver(probe, "probe"), Some("string"));
# Ok::<(), vibescript::Error>(())
```

## Declaration outlines

`outline(source)` parses one source and returns its top-level items in source order: functions, aliases, classes, modules, enums and plain statements. Classes and modules list their instance methods, `def self.` methods, property declarations, aliases, module constants, nested modules and body statements; enums list their members. Each item has a one-based line and Unicode character column, like the positions in compile diagnostics. A declaration's position is its first keyword or modifier, so `private def secret` starts at `private`; enum members and properties start at their names.

Functions, methods and aliases carry signature and body facts. Parameters report their kind, their annotation in the canonical form used by type errors, whether they have a default and whether they assign an instance variable. Bodies report the local names they assign outside blocks, their named rescue clauses and where their last statement begins. These facts describe the syntax only. They are not scopes: a name assigned in one branch is listed even when another path never assigns it.

Outlining uses the compiler's parser with the same source-size and syntax-depth guards, and a source that does not parse returns the same error as `Engine::compile`. It does not validate what compilation would reject after parsing. The walk keeps its own stack, so deeply nested source does not exhaust the native stack.

## Member receivers

`member_receiver(source, name)` classifies the receiver of the first member access called `name`. An editor that wants completions after `value.` can splice a name no script uses at the cursor, so an incomplete line still parses as a member access, and ask for the receiver kind. Literals decide their kind, and so does a parameter of the enclosing function annotated with one non-nullable builtin type. Locals, calls, nullable and union annotations, named types and accesses inside string interpolation report `None`. A syntax error after the access does not matter; one before it does.

## Builtin catalogs

`keywords()` lists the reserved words, and `identifier_char` and `uppercase` classify characters by the same Unicode tables as the lexer. `member_names()` lists the builtin member names per receiver kind in the reference order used for suggestions, followed by the universal helpers each kind answers; the lists match the Go reference's completion tables. The global builtins come from [`vibescript::builtins()`](sessions.md#builtin-names).

`Script::declarations()` also lists top-level declarations, with the byte span of each, for a script that compiled. `outline` differs in what it covers: it needs only a parse, and it describes members, signatures and bodies, which an editor needs while a document is being written.
