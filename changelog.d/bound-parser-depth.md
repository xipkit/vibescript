- **Fixed: bound syntax nesting during compilation.** Excessively nested
  syntax now returns a parse error before parsing or AST traversal can exhaust
  the host stack.
