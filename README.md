<p align="center">
  <img src="docs/logo-dark.svg" alt="Vibescript" height="60">
</p>

# Vibescript

Vibescript is a small, statically typed language for custom logic inside an
app. I want it to be easy to read, easy for AI to write, and predictable for
the app that runs it. The host app chooses what scripts can access and how
much work they can do.

Here’s a small example:

```vibe
def greet(name: string, times: int = 1) -> string
  ("Hello, #{name}! " * times).strip
end

puts greet("Ada")
```

The runtime and command-line tools are written in Rust. Vibescript also runs
on WASI.

## Start here

- [Language guide](docs/language.md) — the language, with examples
- [CLI guide](docs/cli.md) — run, check, format, test, and use the REPL
- [Capabilities](docs/capabilities.md) — connect scripts to your app
- [Editor support](docs/lsp.md)
- [WASI and other platforms](docs/platforms.md)
- [AI authoring study](docs/authoring-evaluation.md)
- [Tested workflow examples](corpus/glue/README.md)

To build and try an example, see the [CLI guide](docs/cli.md).
