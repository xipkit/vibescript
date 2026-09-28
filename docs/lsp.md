# Language server

`vibes lsp` speaks the Language Server Protocol over stdin and stdout. Editors launch it to check and navigate Vibescript documents, offer canonical builtin completions, and apply diagnostic fixes.

```sh
vibes lsp
```

The server lives in the `vibescript-tools` crate as `vibescript_tools::lsp`, behind the default `lsp` feature, so other programs can embed it without the CLI. See [embedding](#embedding).

## Editor setup

Configure an editor client to start `vibes lsp` from `PATH` for `*.vibe` files. For example, in Neovim:

```lua
vim.lsp.start({
  name = "vibes",
  cmd = { "vibes", "lsp" },
  root_dir = vim.fn.getcwd(),
})
```

## Features

| Request or notification | Behavior |
| --- | --- |
| `initialize`, `initialized`, `shutdown`, `exit` | Full-text sync, hover, completion triggered by `.`, signature help triggered by `(` and `,`, definitions, document symbols and formatting. |
| `textDocument/didOpen`, `didChange` | Analyze the full text and publish its diagnostics. Only the last change of a `didChange` counts, as full-text sync sends the whole document. |
| `textDocument/didClose` | Forget the document and publish an empty diagnostics set. |
| `textDocument/hover` | Documentation for the word at the position; see [hover](#hover). |
| `textDocument/completion` | Member methods after a `.`, otherwise keywords, builtins, the document's functions and the enclosing function's parameters and locals. |
| `textDocument/signatureHelp` | Parameter hints for the call around the position on its line, and for a paren-less `assert`. |
| `textDocument/definition` | The declaring line of a top-level function, class, module, method, module constant, enum or enum member in the same document. |
| `textDocument/documentSymbol` | Functions, classes and modules with their methods, constants and nested modules, and enums with their members. |
| `textDocument/formatting` | One full-document edit from `vibescript_tools::format`, the formatter `vibes fmt` uses: it trims trailing spaces and tabs, drops trailing blank lines and ends the text with one newline. |
| `textDocument/codeAction` | A `quickfix` action for each fix of each diagnostic whose range meets the requested range; see [diagnostics](#diagnostics). |

Unknown requests fail with `-32601 method not found`, and requests whose parameters have the wrong shape with `-32602`. Unknown notifications, such as `$/setTrace`, are ignored.

### Diagnostics

Every open and change compiles the document. Syntax failures carry code V0001, severity 1 (error), source `vibes-lsp`, the parser's message, and a range in UTF-16 units. The parser stops at its first error. The range covers the identifier, number or keyword starting at the error, or one character. A hash argument mistaken for a block carries V0002 and a parenthesis fix. Errors without a position, such as an oversized source, are reported at the start of the document.

Other diagnostics come from the [type checker](checker.md) of ADR-007 and ADR-008, which checks the whole document as `vibes check FILE` does, unused declarations included. Each carries its stable code, such as `V0401`, and its range covers the diagnostic's span. Errors prevent compilation; warnings do not. A check stopped by a resource limit publishes one warning (severity 2) at the start of the document, such as `compilation stopped: step quota exceeded (20000000)`.

Only diagnostics in the document itself are published. Required files resolve from the document's directory for `file:` URIs, as `vibes check FILE` resolves them from the script's directory; this is the only file system access the server makes, and it reads the files as saved. Without a directory, as for `untitled:` documents, a `require` is reported as `cannot statically resolve required module`.

The server advertises `codeActionProvider` with the `quickfix` kind and answers `textDocument/codeAction` with one action per fix: its title is the fix's message, its edit a workspace edit of the document, and `isPreferred` is true for a machine-applicable fix and false for a suggestion.

### Hover

A word directly after a namespace receiver resolves to the qualified builtin (`JSON.parse_as`, `Math::PI`). A word reached through `.` on a value shows the member's documentation, merged across receiver kinds when several document it (`length`, or a removed spelling such as `size`), so `price.format` never shows the global `format`. Otherwise builtin, namespace and keyword documentation comes first, then the document's own declarations: a reconstructed signature such as `def add(a: int, b: int = …) -> int` followed by the comment block above the declaration, without `# vibe:` and `# uses:` directives. Duplicate names resolve to the declaration in scope, and a write such as `c.value = 3` prefers the setter. Any other word reads `Vibescript keyword`, `builtin` or `symbol`.

The documentation text is the builtin reference in `tools/src/lsp/reference/`: the builtin, stdlib, string, array, hash, time and duration guides, which document the canonical names with the signature table's exact signatures. Removed member spellings are documented as removed, with their replacements, so a hover on one says what to write; they are never completed. Member completions list the signature table's members, `vibescript::signatures`, rendering each member's signatures as receiver-qualified lines. Tests check that every builtin the runtime registers is documented, that every documented member is a member of the table or a removed spelling of the rename table, and, in `tests/docs.rs`, that every example compiles with static types.

### Completion

After a `.` the server offers member methods. When the syntax decides the receiver's kind, the list narrows to that kind's members: a literal such as `"x".`, `[1].` or `{a: 1}.`, or a parameter annotated with one non-nullable builtin type, such as `s` in `def f(s: string)`. Anything else, including locals, calls, nullable or union annotations and class types, gets the union of every member, each labeled with the kinds that provide it. A dot inside a float literal such as `1.5` does not trigger member completion, but `1.` and `1.days` do. Items carry the member's documentation when only one kind documents it, and the signature table's receiver-qualified signatures, one per receiver and overload, such as `array<T>.fetch(index: int, default?: T, &block?: int -> T) -> T`.

Elsewhere the server offers keywords and builtins with their documentation, the document's top-level functions and aliases, and the parameters and locals of the top-level function around the cursor, with a rescue binding only inside its handler. These come from the last version of the document that compiled, re-anchored to the current lines, so they survive edits that do not parse. The index is built on the first completion request for each version, so the diagnostics path does not pay for it.

## Documents that do not parse

The parser stops at the first error. When a document does not parse, the server outlines each top-level section on its own, splitting before every unindented `def`, `class`, `module`, `enum` or `alias` and after every unindented `end`, and uses the declarations of the sections that parse. When no section parses, it keeps the last outline. Declarations from an older outline are re-anchored to the lines that still declare them, and members move with their class, module or enum; a declaration the text no longer contains is dropped. Completion and signature help keep the last compiled functions in the same way.

## Limits

Each analysis is bounded, so a large or pathological document cannot stall the editor:

- Documents over 1 MiB are not analyzed. They get one diagnostic, `source exceeds maximum size (N > 1048576 bytes)`, and lose their navigation.
- Compilation and the check share a two-second deadline, measured from the start of each analysis. Compilation uses `Engine::compile_with_options`, which charges the compiler's work so the deadline and cancellation reach it; the check uses 20 million steps and 64 MiB. A stopped compilation publishes `compilation stopped: execution deadline exceeded` and keeps the last outline.
- Message bodies over 8 MiB are skipped without being buffered.

Hosts can change these through `Options`.

## Protocol details

- Messages use `Content-Length` framing; header names match without regard to case and other headers are ignored. A missing or malformed `Content-Length`, or a header block over 64 KiB, ends the server with exit status 1 and an error such as `lsp read: missing Content-Length header`, since no later message boundary can be trusted. Input that ends between messages ends the server with status 0.
- A body that is not a JSON-RPC object is skipped. When decoding parameters, absent fields and `null` take zero values, keys match without regard to case when no exact key exists, and a value of the wrong type, such as a fractional line, rejects the request with `-32602`. Absent parameters are an error, while `null` parameters are empty. Request ids are echoed verbatim, and even an `initialize` without an id is answered.
- Output uses stable JSON key order and HTML-safe escapes for `<`, `>` and `&`.
- Positions are UTF-16 code units. Lines end at `\n`, `\r\n` or a bare `\r`, as clients count them.
- `exit` ends the server with status 0 whether or not `shutdown` came first.

The server reads input on a separate thread where threads exist. While it analyzes a document, a `didChange` or `didClose` for the same document cancels the analysis, whose diagnostics would be stale before they were published; a change queued directly behind another change to the same document replaces it unanalyzed; and a `$/cancelRequest` naming a request that is still queued answers it with `-32800 request cancelled`. A client that waits for each reply sees messages handled in order. On WASI, which has no threads, messages are handled strictly in turn.

## Embedding

`vibescript_tools::lsp` offers three layers:

- `Document` analyzes one text and answers diagnostics, hover, completion, definition, symbol and signature-help queries without any protocol. `Document::update` carries declarations across versions as the server does.
- `Server` handles the JSON text of one JSON-RPC message at a time and returns the JSON text of each response and notification to send, so a host can carry it over a WebSocket, in process or any other transport. `Server::exit_requested` reports `exit`.
- `serve(&mut server, input, output, &cancellation)` runs a server over any `Read + Write` pair with `Content-Length` framing; `vibes lsp` calls it with stdin and stdout.

```rust
use vibescript_tools::lsp::{Document, Position};

let document = Document::new("file:///project/main.vibe", "def run\n  puts(\"hi\")\nend\n");
assert!(document.diagnostics().is_empty());
let hover = document.hover(Position::new(1, 3)).unwrap();
assert!(hover.contains("Writes each value"));
```

`Options` sets the check's limits and deadline, a cancellation token, the source size limit, and the directories required files resolve from, for hosts whose documents are not files. The library views the server builds on, declaration outlines and member tables, are described in [editor tooling views](tooling.md).
