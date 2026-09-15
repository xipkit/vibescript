# Options hashes and method calls

Parenthesized instance, class and module methods keep keyword arguments separate from positional arguments. A method with a positional `options` parameter accepts an explicit hash or a keyword named `options`:

```ruby
class Server
  def configure(options)
    options[:retries]
  end
end

server = Server.new
server.configure({retries: 3})          # 3
server.configure(options: {retries: 3}) # 3
server.configure(retries: 3)            # ArgumentError: missing argument options
```

Calls without parentheses, plain function calls, constructors and builtin `send`/`public_send` forwarding can combine keywords into a trailing positional options hash. The target must have a positional or rest parameter available to receive it and no declared keyword or keyword-rest parameters. A keyword matching the next positional parameter binds that parameter directly.

```ruby
server.configure retries: 3
server.send(:configure, retries: 3)
```

The same rules apply through implicit method lookup, safe navigation, ordinary parentheses around a method, rescue-selected calls, splats and attached blocks. The resolved target determines the rule: an ordinary script method named `call`, `send` or `public_send` stays strict when called with parentheses. Builtin exports retain their own keyword contracts. File imports and exported script function values remain part of the unfinished language port.

Argument expressions run before binding. An invalid binding cannot execute defaults, the method body or its block. Safe navigation skips arguments and blocks for a nil receiver. Typed options are checked after the correct binding is selected. Cancellation and exhausted budgets remain uncatchable, and abandoned argument and receiver storage is reclaimed.

The focused reference comparison covers 619 call and parameter combinations plus three previously failing forwarding-audit controls. All 622 expectations are in the shared corpus, with 89 additional uncaught argument rejections. Twenty-six cases have existing [diagnostic wording differences](options-hash-differences.json); their error classes agree with Go. Six resolved type diagnostics now have exact message checks. Native tests also cover callback order, cancellation, exact budgets, stable peak memory across repeated failures and receiver collection cycles.
