# Language server

`vibes lsp` speaks the Language Server Protocol over stdin and stdout. It is a port of the Go reference's `vibes lsp` and answers the same requests the same way; the [comparison](#comparison-with-the-reference) below records where the two differ. Editors launch it; it is not meant to be run by hand.

```sh
vibes lsp
```

The server lives in the `vibescript-tools` crate as `vibescript_tools::lsp`, behind the default `lsp` feature, so other programs can embed it without the CLI. See [embedding](#embedding).

## Editor setup

The reference's Zed extension launches `vibes lsp` from the `PATH`, so it runs this server when this `vibes` is the one found. Other editors need only a generic client that starts `vibes lsp` for `*.vibe` files. In Neovim:

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
| `initialize`, `initialized`, `shutdown`, `exit` | The reference's capabilities: full-text sync, hover, completion triggered by `.`, signature help triggered by `(` and `,`, definitions, document symbols and formatting. |
| `textDocument/didOpen`, `didChange` | Analyze the full text and publish its diagnostics. Only the last change of a `didChange` counts, as full-text sync sends the whole document. |
| `textDocument/didClose` | Forget the document and publish an empty diagnostics set. |
| `textDocument/hover` | Documentation for the word at the position; see [hover](#hover). |
| `textDocument/completion` | Member methods after a `.`, otherwise keywords, builtins, the document's functions and the enclosing function's parameters and locals. |
| `textDocument/signatureHelp` | Parameter hints for the call around the position on its line, and for a paren-less `assert`. |
| `textDocument/definition` | The declaring line of a top-level function, class, module, method, module constant, enum or enum member in the same document. |
| `textDocument/documentSymbol` | Functions, classes and modules with their methods, constants and nested modules, and enums with their members. |
| `textDocument/formatting` | One full-document edit from `vibescript_tools::format`, the formatter `vibes fmt` uses, which matches the reference's: it trims trailing spaces and tabs, drops trailing blank lines and ends the text with one newline. |
| `textDocument/codeAction` | With static types on only: a `quickfix` action for each fix of each diagnostic whose range meets the requested range; see [static diagnostics](#static-diagnostics). |

Unknown requests fail with `-32601 method not found`, and requests whose parameters have the wrong shape with `-32602`. Unknown notifications, such as `$/setTrace`, are ignored.

### Diagnostics

Every open and change compiles the document. A compile error is published in the reference's form: severity 1 (error), source `vibes-lsp`, the parser's bare message, and a range in UTF-16 units. The port's parser stops at its first error, where the reference's parser reports every error it recovers from; that first error has the reference's message and position. The reference spans the offending token; a port error carries only a position, so its range covers the identifier, number or keyword starting there, or one character. Errors without a position, such as an oversized source, are reported at the start of the document.

When the document compiles, the server also runs the static checker over the whole document, as `vibes check FILE` does: top-level code and every function and method declaration, including unused ones. The reference's server reports compile errors only, so these findings are new:

- Known contradictions are errors (severity 1) at the position the checker reports, such as `Return value: expected int, got string`.
- Code the checker cannot analyze yet is information (severity 3), with the checker's message, such as `Analysis of this expression is not implemented`.
- A check that stops at a limit publishes one warning (severity 2) at the start of the document, such as `static check stopped: step quota exceeded (20000000)`.

The checker is deliberately stricter than Go's; see [static checker strictness](compatibility.md#static-checker-strictness). Of the 241 compared documents, 73 get checker findings the reference does not report. Only findings in the document itself are published. Required files resolve from the document's directory for `file:` URIs, as `vibes check FILE` resolves them from the script's directory; this is the only file system access the server makes, and it reads the files as saved. Without a directory, as for `untitled:` documents, `require` is reported as `module paths not configured`.

### Static diagnostics

A server started with `vibes lsp --static`, or created with `Options::static_types`, checks documents in the static language of ADR-007 and ADR-008 instead of running the gradual checker. Every diagnostic then comes from compilation and carries its stable code, such as `V0401`, as the protocol's `code`; its range covers the diagnostic's span. The server advertises `codeActionProvider` with the `quickfix` kind and answers `textDocument/codeAction` with one action per fix: its title is the fix's message, its edit a workspace edit of the document, and `isPreferred` is true for a machine-applicable fix and false for a suggestion. Without static types none of this changes: diagnostics have no code, the capability is not advertised and code action requests fail with `-32601`.

### Hover

Hover follows the reference's lookup order. A word directly after a namespace receiver resolves to the qualified builtin (`JSON.parse_as`, `Math::PI`). A word reached through `.` on a value shows the member's documentation, merged across receiver kinds when several document it (`size`), so `price.format` never shows the global `format`. Otherwise builtin, namespace and keyword documentation comes first, then the document's own declarations: a reconstructed signature such as `def add(a: int, b: int = …) -> int` followed by the comment block above the declaration, without `# vibe:` and `# uses:` directives. Duplicate names resolve to the declaration in scope, and a write such as `c.value = 3` prefers the setter. Any other word reads `Vibescript keyword`, `builtin` or `symbol`.

The documentation text is the reference's own: `scripts/generate-lsp-data.py` copies its builtin, stdlib, string, array, hash, time and duration guides and its member contract registry from the pinned Go module into `tools/src/lsp/reference/` and `tools/src/lsp/contracts.rs`. Tests check that every builtin the runtime registers is documented and that every documented member and contract names a member the runtime dispatches.

### Completion

After a `.` the server offers member methods. When the syntax decides the receiver's kind, the list narrows to that kind's members: a literal such as `"x".`, `[1].` or `{a: 1}.`, or a parameter annotated with one non-nullable builtin type, such as `s` in `def f(s: string)`. Anything else, including locals, calls, nullable or union annotations and class types, gets the union of every member, each labeled with the kinds that provide it. A dot inside a float literal such as `1.5` does not trigger member completion, but `1.` and `1.days` do. Items carry the member's documentation when only one kind documents it, and the reference's receiver-qualified contract signatures, such as `array.fetch(index, default?) { ... }`.

Elsewhere the server offers keywords and builtins with their documentation, the document's top-level functions and aliases, and the parameters and locals of the top-level function around the cursor, with a rescue binding only inside its handler. These come from the last version of the document that compiled, re-anchored to the current lines, so they survive edits that do not parse. The index is built on the first completion request for each version, so the diagnostics path does not pay for it.

## Documents that do not parse

The reference's parser recovers from syntax errors, so its navigation still sees the declarations around a broken one. The port's parser stops at the first error. When a document does not parse, the server outlines each top-level section on its own, splitting before every unindented `def`, `class`, `module`, `enum` or `alias` and after every unindented `end`, and uses the declarations of the sections that parse. When no section parses, it keeps the last outline. Declarations from an older outline are re-anchored to the lines that still declare them, and members move with their class, module or enum; a declaration the text no longer contains is dropped. Completion and signature help keep the last compiled functions in the same way.

## Limits

Each analysis is bounded, so a large or pathological document cannot stall the editor:

- Documents over 1 MiB, the reference's default source limit, are not analyzed. They get one diagnostic, `source exceeds maximum size (N > 1048576 bytes)`, and lose their navigation, as in the reference.
- Compilation and the check share a two-second deadline, measured from the start of each analysis. Compilation uses `Engine::compile_with_options`, which charges the compiler's work so the deadline and cancellation reach it; the check uses 20 million steps and 64 MiB. A stopped compilation publishes `compilation stopped: execution deadline exceeded` and keeps the last outline.
- Message bodies over 8 MiB are skipped without being buffered.

Hosts can change these through `Options`.

## Protocol details

- Messages use `Content-Length` framing; header names match without regard to case and other headers are ignored. A missing or malformed `Content-Length`, or a header block over 64 KiB, ends the server with exit status 1 and an error in the reference's words, such as `lsp read: missing Content-Length header`, since no later message boundary can be trusted. Input that ends between messages ends the server with status 0.
- A body that is not a JSON-RPC object is skipped. Parameters decode as the reference's typed structs do: absent fields and `null` take zero values, keys match without regard to case when no exact key exists, and a value of the wrong type, such as a fractional line, rejects the request with `-32602`. Absent parameters are an error, while `null` parameters are empty. Request ids are echoed verbatim, and even an `initialize` without an id is answered, as in the reference.
- Output uses the reference's JSON: its key order, and HTML-safe escapes for `<`, `>` and `&`.
- Positions are UTF-16 code units. Lines end at `\n`, `\r\n` or a bare `\r`, as clients count them.
- `exit` ends the server with status 0 whether or not `shutdown` came first, as in the reference.

The server reads input on a separate thread where threads exist. While it analyzes a document, a `didChange` or `didClose` for the same document cancels the analysis, whose diagnostics would be stale before they were published; a change queued directly behind another change to the same document replaces it unanalyzed; and a `$/cancelRequest` naming a request that is still queued answers it with `-32800 request cancelled`. A client that waits for each reply, as the comparison does, sees none of this. On WASI, which has no threads, messages are handled strictly in turn.

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

## Comparison with the reference

`scripts/lsp-transcripts.py` drives the Go reference's server and this one in lockstep over the 203 site programs, the 35 reference examples and three documents in `tests/lsp` that cover properties, setters, aliases, nested modules, Unicode names with CRLF line endings and a document broken on open. Each document is opened, then replaced by three unparsable versions: with a broken definition appended, with that and two comment lines inserted above, and cut off halfway. Every version gets an outline, hover and definition at every word, completion at every line start and after every dot, and signature help after every `(` and `,`; the original also gets formatting and hover at every word's end. A session of malformed, unusual and out-of-order messages comes first. The summary is recorded in [lsp-transcripts.json](lsp-transcripts.json):

| Responses | Compared | Identical |
| --- | ---: | ---: |
| Hover | 111,783 | 111,783 |
| Definition | 87,590 | 87,590 |
| Completion | 48,716 | 48,716 |
| Signature help | 18,442 | 18,442 |
| Document symbols | 964 | 964 |
| Formatting | 241 | 241 |
| `didClose` diagnostics | 241 | 241 |
| Protocol session | 34 | 33 |
| `didOpen` diagnostics | 241 | 167 |
| `didChange` diagnostics | 723 | 127 |

Every difference is in diagnostics. On open, 73 documents get checker findings, and the document broken on open reports only the first of the reference's parse errors. After an unparsable edit, 594 documents differ only because the reference reports several parse errors where the port reports the first, which is identical to the reference's, and two halved documents that still parse get checker findings. The remaining protocol difference is the diagnostics for a document broken on open.

The comparison needs Go. The `lsp` [golden corpus](../tests/golden/README.md) records this server's replies to the same sessions over the site programs, the upstream examples and the documents in `tests/lsp`, and `scripts/golden.py` checks them without Go.

## Differences from the reference

Intentional:

- Diagnostics include the static checker's findings, as described in [diagnostics](#diagnostics), and required files resolve from the document's directory for the check.
- Parse errors follow the port's parser: one error per document, with the reference's message and position, and a range covering the word at the error position.
- A body over 8 MiB is skipped and the server continues; the reference exits. The port also rejects header blocks over 64 KiB.
- Queued requests can be cancelled, consecutive changes to one document are analyzed once, and a superseded analysis publishes nothing, where threads exist. The reference handles every message in turn.
- The static check and compilation stop at a deadline and the check at its quotas; the reference has no deadline but checks nothing.

Consequences of the port's parser, which stops at its first error:

- A document that does not parse is outlined section by section. Where a broken section hides a declaration the reference's recovering parser still finds, such as a function whose body is cut off, the port does not list it; where a section no longer parses at all, the port keeps the last outline for it only if no section parses.

