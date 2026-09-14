# Regular expressions

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

`string.match?(pattern, offset = 0)` reports whether a match starts at or after a non-negative character offset. The pattern can be a string or a compiled regex value. An offset past the end returns false, and invalid patterns are still rejected. It rejects keywords and ignores an attached block.

```vibescript
"é ID-12".match?("ID-[0-9]+", 2)
```

The result is true. Offsets count Unicode code points, including one replacement character for each invalid UTF-8 byte. Matches and replacements preserve the subject's original bytes. Regex word boundaries and the default Perl character classes use ASCII definitions; Unicode categories, scripts, aliases and simple folding follow the pinned Go Unicode 17.0.0 tables.

Patterns are limited to 16 KiB; matching subjects, replacement strings and replacement output are limited to 1 MiB. Literal flags may add up to eight bytes to the compiled source. Array scans preserve the fixed 1 MiB output-footprint and 256 MiB potential index-table guards, independently of actual Rust allocation charges. Compilation bounds expanded instruction storage before allocation. Parser stacks, compiled instructions, active matching states, capture slots and output buffers count against the invocation's memory limit. Parsing, matching, copying and state transitions observe work, cancellation and deadline limits. There is no process-global compiled-pattern cache. Keeping a short match does not retain its full subject; an unmatched replacement reuses the original input.

## Regex values

Slash literals compile when evaluated. The `i` flag enables case-insensitive matching; literal `m` lets a dot match newlines. Flags are reported in canonical `im` order. Inline RE2 `m` still controls line anchors. Literal patterns preserve backslashes, allow slashes inside character classes, and do not interpolate.

```vibescript
r = /id-[0-9]+/i
[r.source, r.flags, r.match?("ID-12"), "x ID-12" =~ r]
```

The result is `["id-[0-9]+", "i", true, 2]`. `=~` accepts a string and a regex in either order and returns the first match's character index or nil; `!~` reports no match. Regex `===` and `case` matching accept strings. Equality compares raw source and canonical flags. Inspection, interpolation and string concatenation render slash notation; direct regex `to_s` is not exposed. JSON cannot encode a regex.

`Regexp.new(pattern)` compiles a string. `Regexp.union(*strings)` quotes and combines literal alternatives; no arguments produce a regex that never matches. `Regexp.escape` and `quote` escape regex metacharacters without the pattern-size cap, subject to invocation quotas. `Regexp.last_match` returns nil. These constructors reject keywords and blocks.

The host API provides `Value::regex(pattern_bytes, flags)` and `Value::as_regex()`. Imported regexes share immutable compiled instructions but receive independent invocation charges. Repeated matching reuses compilation, and search scratch is reclaimed after each call.

## Match data

`string.match(pattern, offset = 0)` returns match data or nil. Negative offsets count back from the end; positive offsets past the end clamp to it. `regex.match(text)` returns the same shape without an offset. A string `match` block receives that object and supplies the return value; no match returns nil without invoking the block. A regex value's `match` ignores an attached block.

```vibescript
m = "é ID-12".match(/ID-(?<number>[0-9]+)/)
[m[0], m[:number], m.begin(0), m.end(0), m.pre_match, m.post_match]
```

The result is `["ID-12", "12", 2, 7, "é ", ""]`. Group zero is the whole match; numbered and named indices read captures. Negative group indices count backward, missing captures and out-of-range value indices return nil, and duplicate names select the last participating group. `captures` omits group zero; `named_captures` is a hash. Public field names take precedence over named captures.

`begin(index)` and `end(index)` return character offsets or nil for absent groups and reject out-of-range indices. Their callable values, obtained with `m[:begin]` or `m[:end]`, retain only their offset arrays. Match data's `to_s` entry and interpolation render the whole match. Match data stays protected through nested writes and duplicates; captures copied to a separate variable can be changed independently. This is an explicitly selected difference from Go's inconsistent mutation and clone behavior.

## Scanning

`string.scan(pattern)` returns whole matches when there are no captures, or one capture array per match when there are captures. Missing groups are nil, including captures erased by zero repetitions.

```vibescript
"ID-12 ID-34".scan(/ID-([0-9]+)/)
```

The result is `[["12"], ["34"]]`. A block scan yields the same per-match shape, streams its results and normally returns the original string. Multiple block parameters destructure a capture array. `break`, `next` and nonlocal `return` use the ordinary block rules.

```vibescript
values = []
"ID-12 ID-34".scan(/ID-([0-9]+)/) {|digits| values.push(digits[0])}
values
```

The result is `["12", "34"]`. Scanning preserves anchors, word-boundary context and zero-width advancement. Materialized scans measure output in a first pass and allocate exact result capacity in a second pass, without retaining a table of all match indices. Block scans retain only their current matching state and release discarded block results before the next match.

String substitution methods remain part of the unfinished language port. Rust honors regex anchors even where Go's namespace shortcut skips them; that intentional difference is tracked separately from matching conformance cases.
