### Performance

- Reuse per-call Unicode mapping buffers in `String#swapcase`, reducing temporary allocations while preserving full case expansions and invalid UTF-8 behavior.
