# Regular expressions

`Regex.match(pattern, text)` returns the first matching substring or nil. Patterns use RE2 syntax, including captures, alternation, character classes, counted repetition, anchors and inline `i`, `m`, `s` and `U` flags. Lookaround and pattern backreferences are rejected.

```vibe
Regex.match("ID-[0-9]+", "ID-12 ID-34")
```

The result is `"ID-12"`. Alternatives preserve their order, and quantifiers are greedy unless their flag or trailing question mark selects the opposite behavior.

`Regex.replace(text, pattern, replacement)` replaces the first match. `Regex.replace_all` replaces every non-overlapping match, including zero-width matches according to RE2's advancement rules. Both use dollar references: `$0` for the whole match, `$1` or `${1}` for a numbered group, `$name` or `${name}` for a named group, and `$$` for a literal dollar sign. Missing captures expand to an empty string. Braces delimit a reference from adjacent letters.

```vibe
Regex.replace("ID-12", "ID-([0-9]+)", "X-$1")
```

The result is `"X-12"`.

```vibe
Regex.replace_all("a1 b2", "([a-z])([0-9])", "${2}${1}")
```

The result is `"1a 2b"`. These namespace helpers take string arguments and no keywords or block.

`match?(pattern: string | regex, offset: int = 0) -> bool` reports whether a match starts at or after a non-negative character offset. A string pattern is compiled as a regular expression. An offset past the end returns false, and invalid patterns are still rejected.

```vibe
"é ID-12".match?("ID-[0-9]+", 2)
```

The result is true. Offsets count Unicode code points, including one replacement character for each invalid UTF-8 byte. Matches and replacements preserve the subject's original bytes. Regex word boundaries and the default Perl character classes use ASCII definitions; Unicode categories, scripts, aliases and simple folding follow the pinned Go Unicode 17.0.0 tables.

Patterns are limited to 16 KiB; matching subjects, replacement strings and replacement output are limited to 1 MiB. Literal flags may add up to eight bytes to the compiled source. Array scans preserve the fixed 1 MiB output-footprint and 256 MiB potential index-table guards, independently of actual Rust allocation charges. Compilation bounds expanded instruction storage before allocation. Parser stacks, compiled instructions, active matching states, capture slots and output buffers count against the invocation's memory limit. Parsing, matching, copying and state transitions observe work, cancellation and deadline limits. A pattern that is only a literal, including a case-insensitive one, matches by a linear scan for that literal, and other patterns skip ahead to their literal prefix whenever no match attempt is running, so long literals cost time proportional to the subject rather than to the subject times the pattern. There is no process-global compiled-pattern cache. Keeping a short match does not retain its full subject; an unmatched replacement reuses the original input.

## Regex values

Slash literals compile when evaluated. The `i` flag enables case-insensitive matching; literal `m` lets a dot match newlines. Flags are reported in canonical `im` order. Inline RE2 `m` still controls line anchors. Literal patterns preserve backslashes, allow slashes inside character classes, and do not interpolate.

```vibe
r = /id-[0-9]+/i
[r.source, r.flags, r.match?("ID-12"), "x ID-12" =~ r]
```

The result is `["id-[0-9]+", "i", true, 2]`. `=~` accepts a string and a regex in either order and returns the first match's character index or nil; `!~` reports no match. Regex `===` and `case` matching accept strings. Equality compares raw source and canonical flags. Inspection and interpolation render slash notation; regex has no `to_s`. JSON cannot encode a regex.

`Regex.new(pattern)` compiles a string. `Regex.union(*patterns)` quotes and combines literal alternatives; no arguments produce a regex that never matches. `Regex.escape` escapes regex metacharacters without the pattern-size cap, subject to invocation quotas. The `Regexp` namespace, `Regexp.quote` and `Regexp.last_match` were removed by ADR-008; `Regex` is the one namespace.

The host API provides `Value::regex(pattern_bytes, flags)` and `Value::as_regex()`. Imported regexes share immutable compiled instructions but receive independent invocation charges. Repeated matching reuses compilation, and search scratch is reclaimed after each call.

## Match data

`match(pattern: string | regex, offset: int = 0) -> match_data?` returns match data or nil. Negative offsets count back from the end; positive offsets past the end clamp to it. `regex.match(text)` returns the same result without an offset. Test the result with `!= nil`, or call through `&.`, before using it.

```vibe
m = "é ID-12".match(/ID-(?<number>[0-9]+)/)
if m != nil
  [m[0], m["number"], m.begin(0), m.end(0), m.pre_match, m.post_match]
end
```

The result is `["ID-12", "12", 2, 7, "é ", ""]`. Group zero is the whole match; numbered and named indices read captures. Negative group indices count backward, missing captures and out-of-range value indices return nil, and duplicate names select the last participating group. `captures` omits group zero; `named_captures` is a hash. Public field names take precedence over named captures.

A successful match does not prove that a capture participated. `fetch(group: number | string) -> string` returns a required capture and raises `RuntimeError` (Rust `ErrorKind::Argument`, like array/hash `fetch`) when it is missing or did not participate. It accepts the same numeric indices as `[]`, including negative indices and floats truncated toward zero, and named groups, including names computed at runtime. Duplicate names select the last participating group. An empty participating capture returns `""`. `fetch` selects captures only, so `fetch("captures")` selects that named group, while `["captures"]` still reads the public field. It takes no default or block.

```vibe
m = "ID-12".match(/ID-(?<number>[0-9]+)/)
if m != nil
  m.fetch("number").to_i                 # 12; raises if the capture is missing
end
```

When a missing capture is expected, keep the optional `[]` read and narrow its local before calling string methods:

```vibe
def elapsed_ms(line: string) -> int?
  match = line.match(/ms=([0-9]+)/)
  return nil if match == nil
  digits = match[1]
  return nil if digits == nil
  digits.to_i
end
```

`begin(group)` and `end(group)` return character offsets or nil for absent groups and reject out-of-range indices. `to_s` and interpolation render the whole match. Match data stays protected through nested writes and duplicates; captures copied to a separate variable can be changed independently.

## Scanning

`scan(pattern: string | regex) -> array<string | array<string?>>` returns whole matches when there are no captures, or one capture array per match when there are captures. Missing groups are nil, including captures erased by zero repetitions. Narrow each element before use, for example with `.as(string)` for a pattern without captures.

```vibe
"ID-12 ID-34".scan(/ID-([0-9]+)/)
```

The result is `[["12"], ["34"]]`. Scanning preserves anchors, word-boundary context and zero-width advancement. Scans measure output in a first pass and allocate exact result capacity in a second pass, without retaining a table of all match indices.

## String substitution

Strings support first-match substitution with `sub` and global substitution with `gsub`. A string pattern matches literally, and so does its replacement; a regex literal or `Regex.new(pattern)` selects regular-expression matching. The removed `regex: true` keyword is rewritten to `Regex.new`.

```vibe
text = "bananas"
[text.sub("na", "NA"), text.gsub!("na", "NA"), text]
```

The result is `["baNAnas", "baNANAs", "bananas"]`. Strings remain immutable. The bang forms, `sub!` and `gsub!`, return nil when no match exists and return the replacement result whenever a match exists, even if its bytes are unchanged.

Literal replacements copy the replacement string verbatim. Regex replacements use backslash references:

| Reference | Expansion |
| --- | --- |
| `\0`, `\&` | Whole match |
| `\1` through `\9` | Numbered capture; empty if any named capture is defined |
| `\k<name>` | Named capture; duplicate names select the last participating group |
| `\+` | Last participating capture, considering only named groups when names exist |
| Backslash followed by a backtick | Original text before the match |
| `\'` | Original text after the match |
| `\\` | Literal backslash |

Missing captures expand to an empty string. An unknown or unterminated named reference fails when a match requires its expansion. Unknown escapes and a trailing backslash remain literal. Dollar references remain literal in these string methods. Escape the backslash itself inside a Vibescript string literal.

```vibe
"a1 b2".gsub(/([a-z])([0-9])/, "\\2\\1")
```

The result is `"1a 2b"`.

```vibe
"ID-12 ID-34".gsub(/ID-(?<number>[0-9]+)/, "X-\\k<number>")
```

The result is `"X-12 X-34"`.

A substitution block, `&block: string -> string`, takes the place of the replacement argument. It receives the whole matched substring, including when the pattern has captures, and returns the replacement. `next` supplies the current replacement; `break` and nonlocal `return` leave the call through the ordinary block rules.

```vibe
seen: array<string> = []
result = "ID-12 ID-34".gsub(/ID-[0-9]+/) { |whole|
  seen.push(whole)
  whole.downcase
}
[result, seen]
```

The result is `["id-12 id-34", ["ID-12", "ID-34"]]`.

Empty literal patterns visit Unicode character boundaries, including each invalid UTF-8 byte. Regex substitution preserves assertion context and skips an empty match immediately after a preceding match at that same position. Original subject bytes remain intact outside replacements.

Regex substitutions enforce the 16 KiB pattern and 1 MiB subject, replacement and output guards. Literal substitutions can shrink inputs larger than 1 MiB. Unmatched literal template calls and identical literal pattern/replacement calls reuse the receiver without applying the output guard; block calls still bound their accumulated output, including an unmatched tail.

Templates measure complete expansion before allocating output, then replay matching into exact capacity. Block output grows incrementally within the cap. Conversion measures nested replacement values before copying, rejects provably oversized integers before decimal conversion, and accounts conversion scratch. Search state, output and discarded block values are reclaimed on normal and nonlocal exits. All scans, conversions and copies observe work limits, cancellation and deadlines.

Regex assertions are honored everywhere, including in the namespace helpers.

Match-data protection also applies to nested writes through temporary results and duplicates, such as `m.dup.captures.push(...)`. Block mutators reject these writes before invoking their callbacks. An explicit copy of the capture array itself, `m.captures.dup`, is an independent mutable value.
