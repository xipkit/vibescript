# Keyword arguments and options hashes

Keyword arguments bind only to keyword parameters, which a function declares after a bare `*` or a rest parameter. A hash is a positional value like any other and is passed explicitly:

```vibe
class Server
  def configure(options: hash<string, int>) -> int
    options.fetch("retries", 1)
  end

  def connect(host: string, *, retries: int = 1, verbose: bool = false) -> string
    "#{host} x#{retries}"
  end
end

server = Server.new
server.configure({ retries: 3 })   # 3
server.connect("db", retries: 3)   # "db x3"
server.connect "db", verbose: true # "db x1"
```

Keywords never fold into a trailing positional hash, with or without parentheses, so passing keywords to a function that declares none is a compile error:

```vibe error=V0301,V0302
class Server
  def configure(options: hash<string, int>) -> int
    options.fetch("retries", 1)
  end
end

Server.new.configure(retries: 3)
```

A call must pass every required keyword (V0303) and no keyword the signature does not declare (V0302). Argument expressions run before binding, in source order; an invalid binding cannot execute defaults, the method body or its block. Safe navigation skips arguments and blocks for a nil receiver. Cancellation and exhausted budgets remain uncatchable, and abandoned argument and receiver storage is reclaimed.

Keywords never fold into a trailing positional hash: the options-hash rule the ADR-004 language inherited is gone. The focused comparison of that rule with Go has [recorded wording differences](options-hash-differences.json) that remain part of the `compatibility` golden corpus.
