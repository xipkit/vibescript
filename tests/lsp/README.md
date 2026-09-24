# Language server comparison documents

`scripts/lsp-transcripts.py` drives the Go reference's and this port's `vibes lsp` over these documents, together with the site corpus and the reference examples, and the `lsp` [golden corpus](../golden/README.md) records this port's replies to the same sessions. They cover declarations the corpus rarely uses: class properties, setters, aliases and visibility, nested modules and constants, keyword and rest parameters, annotated receivers for member completion, Unicode names with CRLF line endings, and a document that does not parse when opened.
