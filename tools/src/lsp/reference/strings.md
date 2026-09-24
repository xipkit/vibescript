# String Methods

Vibescript provides several methods for string manipulation.

## Interpolation

Double-quoted strings interpolate single expressions inside `#{...}`:

```vibe
name = "Ada"
"Hello #{name}" # "Hello Ada"
```

Escape the marker with `\#{...}` when literal text is required. Single-quoted
strings do not interpolate.

The expression inside `#{...}` may itself contain double-quoted strings, even
nested interpolations. The interpolation runs until its matching `}`, so quoted
fallbacks and helper calls read naturally:

```vibe
"#{name || "guest"}"        # "guest" when name is nil
"#{["a", "b"].join(", ")}"  # "a, b"
"#{"hi #{inner}"}"          # nested interpolation
```

Interpolated results are built incrementally and bounded by the sandbox step
and memory quotas. A script that grows a string through repeated or large
interpolation (for example `"#{text}#{text}"` in a loop) fails safely with a
memory quota error instead of materializing the oversized result, and a
canceled context stops construction promptly.

## Locale Behavior

String transforms are locale-insensitive and deterministic across supported
platforms. Methods like `upcase`, `downcase`, and `capitalize` use Unicode
case mapping rules and do not depend on host locale settings.

## Basic Methods

### `length`

Alias for `size`, returns the number of characters:

```vibe
"héllo".length  # 5
```

### `bytesize`

Returns the number of bytes in UTF-8 encoding:

```vibe
"hé".bytesize  # 3
```

### `ord`

Returns the codepoint of the first character:

```vibe
"hé".ord  # 104
```

### `chr`

Returns the first character, mirroring Ruby's `String#chr`. An empty string
returns an empty string:

```vibe
"hé".chr  # "h"
"".chr    # ""
```

### `getbyte`

Returns the byte at a byte offset as an integer in the range `0..255`, mirroring
Ruby's `String#getbyte`. The offset is byte-level, not rune-aware, so a multibyte
character occupies more than one offset. A negative offset counts back from the
end, and an offset outside the string returns `nil`:

```vibe
"Aé".getbyte(0)   # 65
"Aé".getbyte(1)   # 195 (first byte of é)
"Aé".getbyte(-1)  # 169 (last byte of é)
"Aé".getbyte(3)   # nil
```

### `byteslice`

Returns a substring selected by byte offsets, mirroring Ruby's
`String#byteslice`. Unlike [`slice`](#slice), offsets are byte-level rather than
rune-aware. It accepts the same three shapes:

- `byteslice(index)` returns the one-byte substring at `index`.
- `byteslice(start, length)` returns up to `length` bytes starting at `start`.
- `byteslice(range)` returns the bytes selected by a range.

Negative offsets count back from the end. An out-of-range start, or a negative
length, returns `nil`; a start exactly at the byte length with a length yields an
empty string:

```vibe
"abc".byteslice(1)       # "b"
"Aé".byteslice(1, 2)     # "é" (the two bytes of é)
"abc".byteslice(1, 10)   # "bc" (length clamps to what is available)
"abc".byteslice(1..3)    # "bc"
"abc".byteslice(-2, 1)   # "b"
"abc".byteslice(3, 2)    # ""
"abc".byteslice(4, 1)    # nil
"abc".byteslice(0, -1)   # nil
```

Because offsets are byte-level, slicing across a UTF-8 boundary returns the raw
bytes verbatim without normalization (so `"Aé".byteslice(1, 1)` is the single
byte `0xC3`, which is not valid UTF-8 on its own), matching Ruby's byte-oriented
semantics.

### `hex`

Interprets the leading characters as a hexadecimal integer, mirroring Ruby's
`String#hex`. Leading whitespace and a single optional `+`/`-` sign are skipped,
an optional `0x`/`0X` prefix is consumed, and single underscores between digits
are treated as separators. Parsing stops at the first byte that is not a hex
digit, and a string with no leading hex digit returns `0`:

```vibe
"ff".hex      # 255
"0xff".hex    # 255
"-1A".hex     # -26
"ff zoo".hex  # 255
"garbage".hex # 0
```

Unlike Ruby, which promotes here as everywhere, Vibescript's lenient
string-to-integer helpers keep their 64-bit domain, so a value outside the
`int64` range raises an `integer out of range` error rather than growing.
(Integer literals and arithmetic do promote to arbitrary precision; see the
[language reference](language_reference.md#values-and-literals).)

### `oct`

Interprets the leading characters as an integer using a base inferred from its
prefix, mirroring Ruby's `String#oct`. The default base is octal, but a
`0x`/`0X`, `0b`/`0B`, `0o`/`0O`, or `0d`/`0D` prefix selects hexadecimal,
binary, octal, or decimal respectively. Leading whitespace, a single optional
sign, and underscore separators are handled like `hex`, parsing stops at the
first byte invalid for the chosen base, and an unparseable string returns `0`:

```vibe
"17".oct     # 15
"0b101".oct  # 5
"0o17".oct   # 15
"0xff".oct   # 255
"0d99".oct   # 99
"-17".oct    # -15
"garbage".oct # 0
```

Like `hex`, a value outside the `int64` range raises an `integer out of range`
error instead of promoting.

### `empty?`

Returns true when the string has no characters:

```vibe
"".empty?      # true
"hello".empty? # false
```

### `inspect`

Returns a double-quoted, escaped debug rendering of the string, distinct from
the raw text that interpolation emits. It escapes `\\`, `\"`, `\n`, `\t`, and the
interpolation marker `\#{`; any other byte (including a carriage return) is
written verbatim, since Vibescript's double-quoted literals have no `\r`, `\xNN`,
or `\uNNNN` escapes. The result therefore round-trips as a Vibescript literal:

```vibe
"hello".inspect   # "\"hello\""
"a\nb".inspect    # "\"a\\nb\""
"say \"hi\"".inspect # "\"say \\\"hi\\\"\""
```

`inspect` is available on every core value kind; see
[Debug Representation](stdlib_core_utilities.md#debug-representation).

### `to_sym` / `intern`

Returns the symbol named by the string, mirroring Ruby's `String#to_sym` and its
alias `String#intern`. Any contents are accepted verbatim, including whitespace,
punctuation, and the empty string:

```vibe
"name".to_sym  # :name
"name".intern  # :name
"a b!".to_sym  # :"a b!"
```

The result is a genuine symbol, so it equals the matching symbol literal but not
the source string:

```vibe
"name".to_sym == :name   # true
"name".to_sym == "name"  # false
```

See `id2name` / `to_s` on a symbol (in `docs/stdlib_core_utilities.md`) for the
reverse direction.

### `to_s` / `string`

`to_s` returns the receiver itself (Ruby's `String#to_s`); `string` is the alias
the [typing docs](typing.md) use as Vibescript's conversion idiom:

```vibe
"id".to_s   # "id"
"id".string # "id"
```

### `to_i` / `to_f`

Parse the string as a base-10 integer or finite float. Surrounding whitespace is
trimmed first. Unlike Ruby's lenient `String#to_i`/`String#to_f` (which skip
trailing characters and return `0`/`0.0` on failure), these are strict like the
global `to_int`/`to_float`: an empty, non-numeric, or (for `to_f`) non-finite
string raises rather than silently yielding zero, so a malformed value cannot
slip through a typed boundary unnoticed.

```vibe
"42".to_i  # 42
" -7 ".to_i # -7
"3.5".to_f # 3.5
```

### `strip`

Removes leading and trailing whitespace:

```vibe
def clean_input(text)
  text.strip
end

clean_input("  hello  ")  # "hello"
```

Like Ruby, `strip` only removes the ASCII whitespace bytes NUL (`\0`), tab
(`\t`), newline (`\n`), vertical tab (`\v`), form feed (`\f`), carriage return
(`\r`), and space (`" "`). NUL is removed from both edges. Unicode spaces such as
NBSP (`U+00A0`), the Ogham space mark (`U+1680`), em space (`U+2003`), and the
byte order mark (`U+FEFF`) are preserved.

### `squish`

Trims leading/trailing whitespace and collapses internal whitespace runs to a
single space:

```vibe
"  hello \n\t world  ".squish # "hello world"
```

### `lstrip`

Removes leading whitespace:

```vibe
"  hello  ".lstrip  # "hello  "
```

Removes the same ASCII whitespace set as `strip` from the start of the string,
including a leading NUL. Unicode spaces are preserved.

### `rstrip`

Removes trailing whitespace:

```vibe
"  hello  ".rstrip  # "  hello"
```

Removes the same ASCII whitespace set as `strip` from the end of the string,
including a trailing NUL. Unicode spaces are preserved.

### `chomp(separator = "\n")`

Removes a trailing separator (default newline). An empty separator removes all
trailing `"\r"` and `"\n"` characters. A `nil` separator means "do not chomp"
and returns the string unchanged, matching Ruby:

```vibe
"line\n".chomp       # "line"
"path///".chomp("/") # "path//"
"line\n\n".chomp("") # "line"
"abc\n".chomp(nil)   # "abc\n"
```

The mutator `chomp!` returns the chomped string when it changes the receiver
and `nil` when nothing changes; a `nil` separator therefore always returns
`nil`.

### `chop`

Removes the last character. A trailing `"\r\n"` is treated as a single record
separator and removed together; otherwise one full character (a Unicode code
point, not a single byte) is removed. An empty string is returned unchanged:

```vibe
"abc".chop      # "ab"
"abc\n".chop    # "abc"
"héllo".chop    # "héll"
"".chop         # ""
```

### `upcase`

Converts the string to uppercase using full Unicode case mapping, so
characters expand where Unicode requires it:

```vibe
def shout(message)
  message.upcase
end

shout("hello")   # "HELLO"
"Straße".upcase  # "STRASSE"
"ﬁle".upcase     # "FILE"
```

Pass `:ascii` to restrict mapping to ASCII letters and leave every other
byte unchanged:

```vibe
"Straße".upcase(:ascii) # "STRAßE"
```

### `downcase`

Converts the string to lowercase using full Unicode case mapping:

```vibe
def normalize(email)
  email.downcase
end

normalize("USER@EXAMPLE.COM")  # "user@example.com"
"İ".downcase                   # "i̇"
```

Pass `:ascii` to restrict mapping to ASCII letters, or `:fold` to apply
Unicode case folding (which normalizes additional forms for case-insensitive
comparison, such as `ß` to `ss`):

```vibe
"STRAßE".downcase(:ascii) # "straße"
"Straße".downcase(:fold)  # "strasse"
```

The `:fold` option is only available on `downcase`; other case methods raise
an error if it is supplied.

### `capitalize`

Titlecases the first character and lowercases the rest using full Unicode
case mapping:

```vibe
"hÉLLo wORLD".capitalize # "Héllo world"
"ǆenan".capitalize       # "ǅenan"
```

Pass `:ascii` to restrict mapping to ASCII letters.

### `swapcase`

Flips the case of each cased character using full Unicode case mapping. This
includes cased characters outside the letter categories, such as circled Latin
letters (`Ⓐ`) and Roman numerals (`Ⅰ`):

```vibe
"Hello VIBE".swapcase # "hELLO vibe"
"Straße".swapcase     # "sTRASSE"
"Ⓐ".swapcase          # "ⓐ"
```

Pass `:ascii` to restrict mapping to ASCII letters. Titlecase digraph
characters (such as `ǅ`) are lowercased rather than split into their component
letters, which is a deliberate divergence from Ruby for those rare codepoints.

### `reverse`

Reverses characters:

```vibe
"héllo".reverse # "olléh"
```

### `start_with?(*prefixes)`

Returns true if the string starts with any of the given prefixes. Requires at
least one prefix, and every prefix must be a string:

```vibe
"vibescript".start_with?("vibe")        # true
"vibescript".start_with?("x", "vibe")   # true
"vibescript".start_with?("x", "script") # false
```

### `end_with?(*suffixes)`

Returns true if the string ends with any of the given suffixes. Requires at
least one suffix, and every suffix must be a string:

```vibe
"vibescript".end_with?("script")        # true
"vibescript".end_with?("x", "script")   # true
"vibescript".end_with?("x", "vibe")     # false
```

### `include?(substring)`

Returns true when `substring` appears in the string:

```vibe
"vibescript".include?("script") # true
```

### `casecmp(other)`

Case-insensitively compares two strings, returning `-1`, `0`, or `1`. Each ASCII
letter `A`-`Z` is folded down to its lowercase form before the byte comparison;
every other byte (including multibyte UTF-8 sequences) is compared ordinally,
matching Ruby's `String#casecmp` (which applies an ASCII `TOLOWER` to each side
in Ruby 2.7 and later). Folding downward keeps the punctuation bytes between `Z`
and `a` (`[`, `\`, `]`, `^`, `_`, and `` ` ``) ordered as Ruby orders them: because
uppercase letters fold into the `a`-`z` range, those punctuation bytes sort below
the folded letters, so `"[".casecmp("A")` is `-1` rather than `1`. Returns `nil`
when `other` is not a string:

```vibe
"abc".casecmp("ABC") # 0
"abc".casecmp("ABD") # -1
"abd".casecmp("ABC") # 1
"[".casecmp("A")     # -1
"z".casecmp("[")     # 1
"abc".casecmp(1)     # nil
```

### `casecmp?(other)`

Returns `true` when two strings are equal under Unicode case folding, `false`
otherwise, and `nil` when `other` is not a string:

```vibe
"abc".casecmp?("ABC")     # true
"héllo".casecmp?("HÉLLO") # true
"abc".casecmp?("ABD")     # false
"abc".casecmp?(1)         # nil
```

Folding uses Unicode simple case folding. Full-fold expansions such as German
`ß` matching `SS` are not applied, so `"ß".casecmp?("SS")` is `false` (Ruby
returns `true`). To normalize those expansions for comparison, use
`downcase(:fold)` on both operands first.

When either operand contains invalid UTF-8, folding falls back to byte-wise
ASCII case folding so that distinct byte sequences stay distinct. This mirrors
Ruby's binary-string path and preserves byte identity, where the Unicode path
would otherwise treat every invalid byte as the same replacement character.

### `match(pattern, offset = 0)`

Regex match returning `[full, capture1, ...]` or `nil`. Captures that did not
participate are `nil`:

```vibe
"ID-12 ID-34".match("ID-([0-9]+)") # ["ID-12", "12"]
```

The optional `offset` is a character (codepoint) position to begin searching
from, so callers can scan from a known point without slicing the receiver first:

```vibe
"hello".match("l", 3)  # ["l"]
"hello".match("l", 4)  # nil
"hello".match("o*", 6) # [""] (clamped to the end; zero-width pattern matches)
```

Offsets behave like Ruby's `String#match`:

- a non-negative offset searches for the first match starting at or after that
  character position;
- a negative offset counts back from the end of the string (so `-1` starts at
  the last character); an offset that lands before the start returns `nil`;
- a positive offset greater than the receiver length is clamped to the length and
  the search runs from the end, matching Ruby: a zero-width-capable pattern (such
  as `o*`, `.*`, `$`, or `\z`) matches the empty string there, while a pattern
  that needs a character to consume returns `nil`. (This is where `match` differs
  from `match?`, which instead returns `false` for an offset past the length.)
- `^`, `\b`, and `\B` keep the full-string context across the offset (the
  characters before the offset still count toward boundary checks), and `\A`
  only matches at the absolute start, so it fails once the offset is past
  position zero.

The offset accepts an integer or a float (truncated toward zero, as in Ruby);
any other type is rejected. An invalid regex is reported regardless of the
offset, and the same regex engine and size guards as `match?` apply.

Given a block, `match` yields the match data (the same `[full, capture1, ...]`
array) when there is a match and returns the block's result; with no match it
returns `nil` and never invokes the block, matching Ruby:

```vibe
"a1".match("([a-z])([0-9])") do |m| m[1] end # "a"
"xyz".match("([a-z])([0-9])") do |m| m[1] end # nil (block not called)
```

### `match?(pattern, offset = 0)`

Allocation-light boolean predicate counterpart to `match`. Returns `true` when
`pattern` has a match at or after the given character offset, otherwise `false`,
without materializing match arrays:

```vibe
"abc".match?("b")           # true
"abc".match?("z")           # false
"abc".match?("ID-[0-9]+")   # false
"abc".match?("b", 1)        # true
"abc".match?("b", 2)        # false
```

The pattern uses the same regex engine and size guards as `match`, so anchors
such as `\A`, `^`, and `\b` keep the full-string context even when an offset is
supplied: the characters preceding the offset still count toward boundary and
anchor checks. The offset is a character (codepoint) position; an offset past
the end of the string yields `false` rather than an error. Unlike Ruby,
`match?` rejects negative offsets.

### `scan(pattern)`

Regex scan returning every non-overlapping match. As in Ruby, the result shape
depends on the number of capture groups in `pattern`:

- No groups: an array of the full match strings.
- One or more groups: an array with one entry per match, each holding that
  match's captured substrings. An optional group that did not participate in
  the match becomes `nil`, distinct from a group that matched an empty string.

```vibe
"ID-12 ID-34".scan("ID-[0-9]+")  # ["ID-12", "ID-34"]
"a1 b2".scan("([a-z])([0-9])")   # [["a", "1"], ["b", "2"]]
"a-b-c".scan("(\\w)(-)?")        # [["a", "-"], ["b", "-"], ["c", nil]]
"abc".scan("z")                   # []
```

Given a block, `scan` yields each match (using the same per-match shape as the
array result above) and returns the receiver string instead of an array,
matching Ruby:

```vibe
out = []
"a1 b2".scan("[a-z][0-9]") do |m| out.push(m) end # returns "a1 b2"
out                                                # ["a1", "b2"]
```

A pattern with many capture groups over a large subject can force the regex engine
to materialize a huge match-index table, so `scan` rejects up front when that table's
worst case would exceed a fixed 256 MiB host cap (see the Guard Limits table in
[stdlib_core_utilities.md](stdlib_core_utilities.md#guard-limits)). The bound is
derived from the subject length and the pattern's minimum match length, so ordinary
sparse scans — a pattern that matches little or nothing over a modest string — are
never rejected regardless of the memory quota.

### `index(substring, offset = 0)`

Returns the first character index for `substring`, or `nil` when not found.
A negative `offset` counts back from the end of the string, so the search
starts at `size + offset`; it yields `nil` when that effective offset falls
before the start of the string:

```vibe
"héllo hello".index("llo")    # 2
"héllo hello".index("llo", 6) # 8
"héllo hello".index("zzz")    # nil
"hello".index("l", -3)        # 2
"hello".index("l", -9)        # nil
```

### `rindex(substring, offset = size)`

Returns the last character index for `substring`, or `nil` when not found.
A negative `offset` counts back from the end of the string and the backward
search starts at `size + offset`; it yields `nil` when that effective offset
falls before the start of the string:

```vibe
"héllo hello".rindex("llo")    # 8
"héllo hello".rindex("llo", 4) # 2
"hello".rindex("l", -2)        # 3
"hello".rindex("l", -9)        # nil
```

### Bracket access (`string[selector]`)

Bracket access mirrors Ruby's `String#[]` and is rune-aware, so it operates on
characters rather than bytes. It is the operator spelling of the integer, range,
and start/length forms of [`slice`](#sliceselector-length--nil):

- `string[index]` returns a single character; a negative index counts back from
  the end, and an index at or past the length returns `nil`;
- `string[start, length]` returns up to `length` characters starting at `start`
  (negative `start` counts from the end), with the same empty-string, clamping,
  and `nil` rules as `slice`;
- `string[range]` returns the selected substring, with Ruby-compatible negative
  bounds.

```vibe
"abcd"[-1]     # "d"
"abcd"[10]     # nil
"abcd"[1, 2]   # "bc"
"abcd"[1..2]   # "bc"
"abcd"[1...2]  # "b"
"héllo"[1]     # "é"
```

Strings are immutable, so there is no bracket assignment; build a new string
with [`slice`](#sliceselector-length--nil), [`sub`](#subpattern-replacement-regex-false),
or concatenation instead.

### `slice(selector, length = nil)`

Extracts a character or substring, returning `nil` when the selector falls
outside the string. Indexing is rune-aware. `slice` accepts the same selector
shapes as Ruby's `String#slice`:

- an integer index returns a single character; a negative index counts back
  from the end, and an index at or past the length returns `nil`;
- an integer `start` with a `length` returns up to `length` characters from
  `start` (negative `start` counts from the end). A `start` exactly at the
  length yields `""`, a negative `length` returns `nil`, and an oversized
  `length` is clamped to the end of the string;
- a range returns the selected substring, with Ruby-compatible negative bounds;
- a substring returns that substring when it is contained, otherwise `nil`.

```vibe
"héllo".slice(1)      # "é"
"héllo".slice(-1)     # "o"
"héllo".slice(99)     # nil
"héllo".slice(1, 3)   # "éll"
"héllo".slice(-3, 2)  # "ll"
"héllo".slice(1..-1)  # "éllo"
"héllo".slice(1...3)  # "él"
"héllo".slice("llo")  # "llo"
"héllo".slice("x")    # nil
```

### `sub(pattern, replacement, regex: false)`

Replaces the first occurrence of `pattern`:

```vibe
"bananas".sub("na", "NA") # "baNAnas"
"ID-12 ID-34".sub("ID-[0-9]+", "X", regex: true) # "X ID-34"
```

With `regex: true`, the replacement string uses Ruby-style backreferences (see
[Replacement backreferences](#replacement-backreferences)):

```vibe
"abc123".sub("([a-z]+)([0-9]+)", "\\2-\\1", regex: true) # "123-abc"
```

A Ruby-style regex literal also selects regex matching by itself (the `regex:`
keyword is rejected alongside it, since a regex pattern is never literal). The
same applies to `gsub`, `match`, `match?`, and `scan`:

```vibe
"ID-12 ID-34".sub(/ID-[0-9]+/, "X") # "X ID-34"
"abc123".sub(/([a-z]+)([0-9]+)/, "\\2-\\1") # "123-abc"
```

Given a block instead of a replacement argument, the block receives the matched
substring and its result (coerced to a string) replaces the first match:

```vibe
"hello".sub("l") do |m| m.upcase end                 # "heLlo"
"abc".sub("[bc]", regex: true) do |m| m.upcase end   # "aBc"
```

The block form honors the same `regex` keyword (defaulting to literal matching),
so use `regex: true` when the pattern should be interpreted as a regular
expression. Passing both a replacement argument and a block is an error.

### `gsub(pattern, replacement, regex: false)`

Replaces all occurrences of `pattern`:

```vibe
"bananas".gsub("na", "NA") # "baNANAs"
"ID-12 ID-34".gsub("ID-[0-9]+", "X", regex: true) # "X X"
"a1b2".gsub("([a-z])([0-9])", "\\2\\1", regex: true) # "1a2b"
```

Given a block instead of a replacement argument, the block receives each matched
substring and its result (coerced to a string) replaces that match:

```vibe
"hello".gsub("l") do |m| m.upcase end                # "heLLo"
"abc".gsub("[bc]", regex: true) do |m| m.upcase end  # "aBC"
```

As with `sub`, the block form honors the `regex` keyword (defaulting to literal
matching), and passing both a replacement argument and a block is an error. The
`sub!` and `gsub!` variants accept the same block form. Matching Ruby, they
return the rewritten string whenever the pattern matched -- even when the
replacement reproduces the original text, as in `"a".sub!("a") do |m| m end`
-- and return `nil` only when the pattern never matched.

#### Replacement backreferences

For `sub`, `sub!`, `gsub`, and `gsub!` with `regex: true`, the replacement
string follows Ruby's substitution syntax. A backslash introduces a
backreference; every other character (including `$`) is copied verbatim, so
`$1` and `$&` are literal text rather than group references:

| Sequence   | Expands to                                            |
| ---------- | ----------------------------------------------------- |
| `\0`, `\&` | the entire match                                      |
| `\1`–`\9`  | the matching capture group (single digit only)        |
| `` \` ``   | the text before the match (pre-match)                 |
| `\'`       | the text after the match (post-match)                 |
| `\+`       | the last capture group that participated in the match |
| `\k<name>` | the named capture group `name`                        |
| `\\`       | a literal backslash                                   |

A backslash followed by anything else is kept literally together with that
character (so `\z` stays `\z`). Numbered or `\+` references to groups that did
not participate expand to the empty string. A `\k<name>` that names a group the
pattern never defines raises an error, as does an unterminated `\k<name`.

As in Ruby, once a pattern defines any named capture group the numbered
references `\1`–`\9` are disabled and expand to the empty string, even for
groups that participated; reach those groups with `\k<name>` instead. The
whole-match (`\0`, `\&`), pre/post-match (`` \` ``, `\'`), and `\k<name>`
references keep working in that mode. For example
`"John Smith".sub("(?<first>\\w+) (?<last>\\w+)", "\\2, \\1", regex: true)`
yields `", "`.

When a pattern reuses a group name (for example `(?<x>a)(?<x>b)` or
`(?<x>foo)|(?<x>bar)`), `\k<name>` expands to the last occurrence that
participated in the match, matching Ruby: `(?<x>a)(?<x>b)` over `"ab"` expands
`\k<x>` to `"b"`, and `(?<x>a)(?<x>b)?(?<x>c)` over `"ac"` expands it to `"c"`.
If the name exists but no occurrence participated, the reference expands to the
empty string.

```vibe
"abc".sub("b", "<\\&>", regex: true)   # "a<b>c"
"abc".sub("b", "<$&>", regex: true)    # "a<$&>c" ($& is literal)
"John Smith".sub("(?<first>\\w+) (?<last>\\w+)", "\\k<last>, \\k<first>", regex: true) # "Smith, John"
```

### `delete_prefix(prefix)`

Removes the prefix when present:

```vibe
"unhappy".delete_prefix("un") # "happy"
```

### `delete_suffix(suffix)`

Removes the suffix when present:

```vibe
"report.csv".delete_suffix(".csv") # "report"
```

## Padding

`width` is measured in characters (Unicode code points), not bytes or display
columns, so multibyte characters each count as one. When `width` is less than or
equal to the receiver's length, the receiver is returned unchanged. A `Float`
width is truncated toward zero. The pad string defaults to a single space and
must not be empty; it is repeated and then truncated at a character boundary to
fill the requested span.

### `str * count`

Repeats the string `count` times, as in Ruby. This is the usual way to build a
separator, indent, or table rule at a computed width:

```vibe
"-" * 20            # "--------------------"
"ab" * 3            # "ababab"
"ab" * 0            # ""
"  " * depth        # indentation for a nesting level
```

A `Float` count is truncated toward zero (`"ab" * 2.0` is `"abab"`), and a
negative count is reported rather than treated as zero. Repetition is not
commutative: `3 * "ab"` is an error, as in Ruby.

### `center(width, pad = " ")`

Centers the string, padding both sides. When the padding cannot be split evenly,
the extra character goes on the right:

```vibe
"hi".center(6, "-")     # "--hi--"
"hi".center(5)          # " hi  "
"hi".center(10, "12345") # "1234hi1234"
```

### `ljust(width, pad = " ")`

Left-justifies the string, padding on the right:

```vibe
"hi".ljust(5, ".") # "hi..."
"hi".ljust(7, "ab") # "hiababa"
```

### `rjust(width, pad = " ")`

Right-justifies the string, padding on the left:

```vibe
"hi".rjust(5, ".") # "...hi"
"hi".rjust(7, "ab") # "ababahi"
```

## Compatibility Methods

Vibescript strings are immutable, so mutating-style Ruby methods return a new string.

### `clear`

Returns an empty string:

```vibe
"hello".clear # ""
```

### `concat(*strings)`

Appends one or more strings:

```vibe
"hello".concat           # "hello"
"he".concat("llo", "!") # "hello!"
```

### `prepend(*strings)`

Prepends one or more strings, in order:

```vibe
"abc".prepend("z")      # "zabc"
"abc".prepend("y", "z") # "yzabc"
"abc".prepend           # "abc"
```

### `insert(index, string)`

Inserts `string` into a copy of the receiver at a character index, matching
Ruby's `String#insert`. A non-negative index inserts before the character at
that position, so the valid range is `0` to the string's length (a value equal
to the length appends). A negative index inserts after the character it selects,
so `-1` appends and the valid range is `-1` down to `-(length + 1)`. The index
counts characters, not bytes, so it works the same way for multibyte text. An
out-of-range index raises an error.

```vibe
"abc".insert(1, "X")   # "aXbc"
"abc".insert(3, "X")   # "abcX"
"abc".insert(-1, "X")  # "abcX"
"abc".insert(-2, "X")  # "abXc"
"café".insert(2, "Z")  # "caZfé"
```

### `replace(replacement)`

Returns `replacement`:

```vibe
"old".replace("new") # "new"
```

### Bang aliases

Strings are immutable values, so — unlike the array and hash mutators — the
bang methods cannot rewrite the receiver in place; rebind the result
(`s = s.upcase! || s`) when you need the change to stick. They keep Ruby's
return contract: the transformed string when a change was made, `nil` when
nothing changed.

- `strip!`, `lstrip!`, `rstrip!`, `chomp!`, `chop!`
- `squish!`
- `delete!`, `delete_prefix!`, `delete_suffix!`
- `tr!`, `squeeze!`
- `upcase!`, `downcase!`, `capitalize!`, `swapcase!`, `reverse!`

`sub!` and `gsub!` key their result off the match instead of a byte
comparison: they return the rewritten string whenever the pattern matched —
even when the replacement reproduces the original text — and `nil` only when
the pattern never matched.

## Splitting

### `split(separator = nil, limit = 0)`

Splits a string into an array of strings.

**Without arguments:** Splits on runs of ASCII whitespace and removes empty
entries, mirroring Ruby's default `String#split`. Leading and trailing
whitespace is discarded and a blank string yields an empty array:

```vibe
"one two  three".split  # ["one", "two", "three"]
"  a  b  ".split        # ["a", "b"]
```

Only the ASCII whitespace bytes split fields: space, tab, newline, vertical
tab, form feed, and carriage return. Wider Unicode whitespace such as the
non-breaking space (`U+00A0`) or the em space (`U+2003`) is kept inside the
field rather than acting as a separator, matching Ruby instead of treating the
full Unicode whitespace table as delimiters:

```vibe
"a b".split  # ["a b"] (non-breaking space is preserved)
```

**With separator:** Splits on the specified string. By default trailing empty
fields are removed, matching Ruby's default `String#split`:

```vibe
"a,b,c".split(",")        # ["a", "b", "c"]
"path/to/file".split("/") # ["path", "to", "file"]
"a,b,".split(",")         # ["a", "b"] (trailing empty removed)
```

**With an empty separator:** Splits the string into its individual characters
(runes), matching Ruby's `String#split("")`:

```vibe
"abc".split("")   # ["a", "b", "c"]
"héllo".split("") # ["h", "é", "l", "l", "o"]
```

**With an explicit `nil` separator:** Behaves exactly like the no-argument
form, splitting on runs of ASCII whitespace, matching Ruby's
`String#split(nil)`:

```vibe
" a  b ".split(nil) # ["a", "b"]
```

**With a single space separator:** A separator of exactly `" "` is Ruby's AWK
whitespace mode rather than a literal split on the space character. It collapses
runs of ASCII whitespace, discards leading whitespace, and honors the limit just
like the `nil` form, so it never produces a leading empty field:

```vibe
" a  b ".split(" ")    # ["a", "b"]
" a  b ".split(" ", 2) # ["a", "b "]
```

Any other non-string separator raises an error.

**With a `limit`:** The optional second argument controls how many fields are
returned and whether trailing empty fields are kept, matching Ruby:

- A **positive** limit returns at most that many fields, leaving the remainder
  unsplit in the final field. A limit of `1` returns the whole string unchanged.
- The default **`0`** removes trailing empty fields.
- A **negative** limit preserves every field, including trailing empties.

```vibe
"a,b,c,d".split(",", 2)  # ["a", "b,c,d"]
"a,b,c".split(",", 1)    # ["a,b,c"]
"a,b,".split(",", -1)    # ["a", "b", ""]
"a,b,".split(",", 0)     # ["a", "b"]
```

The limit applies to every separator mode. With the whitespace default a
positive limit keeps the unsplit remainder in the final field, and a non-zero
limit preserves a single trailing empty field when the string ends in
whitespace:

```vibe
"  a b c  ".split(nil, 2)  # ["a", "b c  "]
"  a b c  ".split(nil, -1) # ["a", "b", "c", ""]
"abc".split("", -1)        # ["a", "b", "c", ""]
```

An empty string always yields an empty array, regardless of the limit. A
non-integer limit raises an error.

### `partition(separator)`

Splits the string around the **first** occurrence of `separator`, returning a
three-element array of the text before the separator, the separator itself, and
the text after it:

```vibe
"abc=def=ghi".partition("=") # ["abc", "=", "def=ghi"]
```

When the separator is not found, the whole string is returned as the first
element with two empty trailing elements. An empty separator matches at the very
start:

```vibe
"no-sep".partition("=") # ["no-sep", "", ""]
"abc".partition("")     # ["", "", "abc"]
```

The separator must be a string.

### `rpartition(separator)`

Splits the string around the **last** occurrence of `separator`, returning a
three-element array of the text before the separator, the separator itself, and
the text after it:

```vibe
"abc=def=ghi".rpartition("=") # ["abc=def", "=", "ghi"]
```

When the separator is not found, the whole string is returned as the last
element with two empty leading elements. An empty separator matches at the very
end:

```vibe
"no-sep".rpartition("=") # ["", "", "no-sep"]
"abc".rpartition("")     # ["abc", "", ""]
```

The separator must be a string.

### `chars`

Returns an array of the string's Unicode characters, one entry per code point.
This is rune-aware, matching the behavior of `length` and `slice`:

```vibe
"abc".chars # ["a", "b", "c"]
"héllo".chars # ["h", "é", "l", "l", "o"]
"".chars # []
```

### `lines`

Splits the string into lines using `"\n"` as the separator, keeping the trailing
newline on each line. A trailing newline does not produce a final empty line,
and an empty string yields no lines:

```vibe
"a\nb".lines   # ["a\n", "b"]
"a\nb\n".lines # ["a\n", "b\n"]
"".lines       # []
```

Only `\n` ends a line. Carriage returns are never treated as separators and are
preserved verbatim, so a Windows-style `\r\n` line ending keeps the `\r`
attached to the line that precedes the `\n` (input on the left contains literal
carriage-return bytes):

```text
"a\r\nb".lines # ["a\r\n", "b"]
"a\rb".lines   # ["a\rb"]
```

### `bytes`

Returns an array of the string's bytes as integers in the range `0..255`. This
is byte-level, not rune-aware: a multibyte character expands to one entry per
UTF-8 byte, so `bytes.size` equals [`bytesize`](#bytesize), not
[`length`](#length):

```vibe
"abc".bytes   # [97, 98, 99]
"héllo".bytes # [104, 195, 169, 108, 108, 111]
"".bytes      # []
```

The raw bytes are returned verbatim. Strings carrying invalid UTF-8 (only
reachable through host-provided values, since Vibescript literals cannot encode
them) are not normalized, matching Ruby's `String#bytes`.

### `codepoints`

Returns an array of the string's Unicode code points as integers, mirroring
Ruby's `String#codepoints`. This is rune-aware (the integer counterpart to
[`chars`](#chars)): a multibyte character contributes a single code point, so
`codepoints.size` equals [`length`](#length), not [`bytesize`](#bytesize):

```vibe
"abc".codepoints # [97, 98, 99]
"Aé".codepoints  # [65, 233]
"😀".codepoints   # [128512]
"".codepoints    # []
```

### `each_char`

Yields each Unicode character to a block without first materializing an array,
the streaming counterpart to [`chars`](#chars). Characters are whole code points,
matching the rune-aware behavior of `length`, `slice`, and `chars`. The block's
return value is ignored, and `each_char` returns the receiver string:

```vibe
out = []
"héllo".each_char { |c| out = out + [c] }
out # ["h", "é", "l", "l", "o"]
```

A block is required. Vibescript has no `Enumerator`, so calling `each_char`
without a block reports `string.each_char requires a block` rather than returning
an enumerator.

### `each_byte`

Yields each byte as an integer in the range `0..255` without first materializing
an array, the streaming counterpart to [`bytes`](#bytes). Iteration is
byte-level, so a multibyte character is yielded one UTF-8 byte at a time. The
block's return value is ignored, and `each_byte` returns the receiver string:

```vibe
out = []
"é".each_byte { |b| out = out + [b] }
out # [195, 169]
```

As with `each_char`, a block is required; calling `each_byte` without a block
reports `string.each_byte requires a block`.

### `each_codepoint`

Yields each Unicode code point as an integer without first materializing an
array, the streaming counterpart to [`codepoints`](#codepoints). Iteration is
rune-aware, so a multibyte character is yielded as a single code point. The
block's return value is ignored, and `each_codepoint` returns the receiver
string:

```vibe
out = []
"Aé".each_codepoint { |c| out = out + [c] }
out # [65, 233]
```

As with `each_char`, a block is required; calling `each_codepoint` without a
block reports `string.each_codepoint requires a block`.

### `each_line`

Yields each line to a block without first materializing an array, the streaming
counterpart to [`lines`](#lines). Lines retain their trailing `\n` and follow the
same separator rules as `lines`: only `\n` ends a line, a trailing newline does
not produce a final empty line, and a Windows-style `\r\n` keeps the `\r`
attached. The block's return value is ignored, and `each_line` returns the
receiver string:

```vibe
out = []
"a\nb\n".each_line { |l| out = out + [l] }
out # ["a\n", "b\n"]
```

As with `each_char`, a block is required; calling `each_line` without a block
reports `string.each_line requires a block`.

## Templating

### `template(context, strict: false)`

Interpolates `{{name}}` placeholders from a hash context. Dot paths can access
nested hashes (`{{user.name}}`).

```vibe
tpl = "Player {{user.name}} scored {{user.score}}"
ctx = { user: { name: "Alex", score: 42 } }

tpl.template(ctx) # "Player Alex scored 42"
```

When `strict: true`, missing placeholders raise an error instead of being left
unchanged.

## Example: Text Processing

```vibe
def parse_tags(input)
  tags = input.strip.downcase.split(",")
  tags = tags.map do |tag|
    tag.strip
  end
  tags.select do |tag|
    tag != ""
  end
end

parse_tags("  Ruby, Go,  Python  ")
# ["ruby", "go", "python"]
```
