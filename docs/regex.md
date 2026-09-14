# Regular-expression helpers

`Regex.match(pattern, text)` returns the first matching substring or nil. Patterns use RE2 syntax, including captures, alternation, character classes, counted repetition, anchors and inline `i`, `m`, `s` and `U` flags. Lookaround and pattern backreferences are rejected.

```vibescript
Regex.match("ID-[0-9]+", "ID-12 ID-34")
```

The result is `"ID-12"`. Alternatives preserve their order, and quantifiers are greedy unless their flag or trailing question mark selects the opposite behavior.

`Regex.replace(text, pattern, replacement)` replaces the first match. `Regex.replace_all` replaces every non-overlapping match, including zero-width matches according to RE2's advancement rules. Both use dollar references: `$0` for the whole match, `$1` or `${1}` for a numbered group, `$name` or `${name}` for a named group, and `$$` for a literal dollar sign. Missing captures expand to an empty string. Braces delimit a reference from adjacent letters.

```vibescript
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1")
```

The result is `"X-12"`.

```vibescript
Regex.replace_all("a1 b2", "([a-z])([0-9])", "${2}${1}")
```

The result is `"1a 2b"`. These namespace helpers require string arguments and reject keywords and blocks. Namespace aliases and scoped bindings behave like other builtin namespaces.

`string.match?(pattern, offset = 0)` reports whether a match starts at or after a non-negative character offset. An offset past the end returns false, and invalid patterns are still rejected. It rejects keywords and ignores an attached block.

```vibescript
"é ID-12".match?("ID-[0-9]+", 2)
```

The result is true. Offsets count Unicode code points, including one replacement character for each invalid UTF-8 byte. Matches and replacements preserve the subject's original bytes. Regex word boundaries and the default Perl character classes use ASCII definitions; Unicode categories, scripts, aliases and simple folding follow the pinned Go Unicode 17.0.0 tables.

Patterns are limited to 16 KiB; subjects, replacements and output are limited to 1 MiB. Compilation bounds expanded instruction storage before allocation. Parser stacks, compiled instructions, active matching states, capture slots and output buffers count against the invocation's memory limit. Parsing, matching, copying and state transitions observe work, cancellation and deadline limits. There is no process-global compiled-pattern cache. Keeping a short match does not retain its full subject; an unmatched replacement reuses the original input.

Regex literals and values, `Regexp` constructors, match-data objects, string `match`/`scan`, and string substitution methods remain part of the unfinished language port. The Go namespace shortcut's handling of anchored literal patterns is tracked separately from matching conformance cases.
