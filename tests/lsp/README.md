# Language server documents

The `lsp` [golden corpus](../golden/README.md) drives `vibes lsp` over these documents, together with the site corpus and the upstream examples, in the sessions `scripts/lsp_sessions.py` generates, and records the replies. They cover declarations the corpus rarely uses: class properties, setters, aliases and visibility, nested modules and constants, keyword and rest parameters, annotated receivers for member completion, Unicode names with CRLF line endings, and a document that does not parse when opened.
