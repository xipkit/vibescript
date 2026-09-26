# Strings

A `string` is an immutable sequence of bytes, usually UTF-8 text. Every method
returns a new value and leaves the receiver unchanged, so a transformation is
kept by assigning its result: `name = name.strip`.

Most methods count characters, meaning Unicode code points: `length`,
indexing, `slice`, `index`, `center` and friends all treat `"é"` as one
character. The byte-level methods (`bytesize`, `bytes`, `getbyte`,
`byteslice`, `each_byte`) count UTF-8 bytes instead. A string that holds
invalid UTF-8, which only a host can supply, is read as one character per
invalid byte.

## Literals and interpolation

Double-quoted strings process escapes such as `\n`, `\t`, `\"`, `\\`,
`\x41` and `\u00e9`, and interpolate any expression inside `#{...}`. The
expression may contain its own quoted strings and interpolations. Single-quoted
strings are taken literally, and `\#{` writes the marker itself.

```vibe
name = "Ada"
items = ["a", "b"]
nickname: string? = nil

"Hello #{name}"                                  # "Hello Ada"
"#{items.join(", ")} (#{items.length})"          # "a, b (2)"
"Hi #{nickname == nil ? "guest" : nickname}"     # "Hi guest"
'no #{interpolation}'                            # "no \#{interpolation}"
```

Interpolation renders each value as `to_s` does: `nil` becomes `""` and
collections render as their literals. Results are built incrementally under
the call's step and memory limits, so a string that grows without bound fails
with a quota error instead of exhausting the host.

## Operators

- `a + b` concatenates two strings. The other operand must be a string too:
  convert numbers explicitly, as in `"total: " + count.to_s`, or interpolate.
- `text * count` repeats the string. A float count is truncated toward zero; a
  negative count raises.
- `pattern % values` formats like `format(pattern, *values)`; `values` is an
  array, or a single value for one placeholder.
- `==`, `<`, `<=`, `>`, `>=` and `<=>` compare strings bytewise.
- `text =~ regex` returns the character index of the first match or `nil`, and
  `text !~ regex` is `true` when there is no match.

```vibe
count = 3
"total: " + count.to_s   # "total: 3"
"-" * 5                  # "-----"
"%s:%03d" % ["id", 7]    # "id:007"
"%.2f" % 1.234           # "1.23"
"apple" < "banana"       # true
"ID-12" =~ /[0-9]+/      # 3
```

### Indexing

`text[index]`, `text[start, length]` and `text[range]` read characters, with the
same rules as `slice` below. The result is `string?`: an index outside the
string gives `nil`. Strings have no index assignment; build a new string with
`sub`, `insert` or concatenation.

```vibe
word = "héllo"
word[1]       # "é"
word[-1]      # "o"
word[1, 3]    # "éll"
word[1..2]    # "él"
word[10]      # nil
```

## Size and characters

### `length -> int`

The number of characters (Unicode code points).

```vibe
"héllo".length  # 5
"é🙂".length    # 2
```

### `bytesize -> int`

The number of bytes in the string's UTF-8 encoding.

```vibe
"hé".bytesize  # 3
```

### `empty? -> bool`

Whether the string has no characters.

```vibe
"".empty?       # true
" ".empty?      # false
```

### `chars -> array<string>`

The characters, one string per code point.

### `bytes -> array<int>`

The bytes as integers from 0 to 255, one per UTF-8 byte.

### `codepoints -> array<int>`

The Unicode code points as integers, one per character.

```vibe
"héllo".chars    # ["h", "é", "l", "l", "o"]
"hé".bytes       # [104, 195, 169]
"Aé".codepoints  # [65, 233]
```

### `ord -> int`

The code point of the first character.

### `chr -> string`

The first character, or `""` for an empty string.

```vibe
"hé".ord  # 104
"hé".chr  # "h"
```

### `getbyte(index: int) -> int?`

The byte at a byte offset, from 0 to 255. A negative offset counts back from the
end, and an offset outside the string gives `nil`.

```vibe
"Aé".getbyte(0)   # 65
"Aé".getbyte(-1)  # 169, the last byte of "é"
"Aé".getbyte(3)   # nil
```

### `byteslice(start: int | range, length?: int) -> string?`

A substring selected by byte offsets: the byte at `start`, `length` bytes from
`start`, or the bytes a range selects. A negative `start` counts back from the
end and `length` is clamped to what is available. A start past the end or a
negative length gives `nil`, and a start exactly at the end gives `""`. Bytes
are copied verbatim, so cutting through a multibyte character yields invalid
UTF-8.

```vibe
"abc".byteslice(1)      # "b"
"Aé".byteslice(1, 2)    # "é"
"abc".byteslice(1, 10)  # "bc"
"abc".byteslice(1..3)   # "bc"
"abc".byteslice(3, 2)   # ""
"abc".byteslice(4, 1)   # nil
```

## Conversion

### `to_i -> int`

Parses the whole string, after trimming surrounding whitespace, as a base-10
integer of any size. Anything else, including an empty string or trailing
text, raises.

### `to_f -> float`

Parses the whole trimmed string as a finite decimal or hexadecimal float.
Invalid text and values out of float range raise.

```vibe
"42".to_i       # 42
" -7 ".to_i     # -7
"3.5".to_f      # 3.5
```

### `hex -> int`

Reads a leading hexadecimal number: leading whitespace, one optional sign and
an optional `0x` prefix are skipped, single underscores may separate digits,
and parsing stops at the first character that is not a hex digit. Text without
leading digits gives `0`. Results outside the signed 64-bit range raise.

### `oct -> int`

Reads a leading integer whose base comes from its prefix: octal by default,
`0x` hexadecimal, `0b` binary, `0o` octal and `0d` decimal. Whitespace, signs,
underscores and the stopping rule are as for `hex`.

```vibe
"ff".hex        # 255
"-1A".hex       # -26
"ff zoo".hex    # 255
"garbage".hex   # 0
"17".oct        # 15
"0b101".oct     # 5
"0d99".oct      # 99
```

### `to_sym -> symbol`

The symbol with the same bytes. Any content is accepted, including spaces and
the empty string. A symbol is not equal to the string it came from.

```vibe
"draft".to_sym            # :draft
"draft".to_sym == :draft  # true
"draft".to_sym == "draft" # false
```

### `to_s -> string`

The string itself.

### `inspect -> string`

A double-quoted, escaped rendering that reads back as a string literal. It
escapes `\`, `"`, newlines, tabs and the interpolation marker, and writes
other bytes as they are.

```vibe
"say \"hi\"".inspect  # "\"say \\\"hi\\\"\""
"a\nb".inspect        # "\"a\\nb\""
```

## Comparison

### `casecmp(other: string) -> int`

Compares two strings ignoring ASCII case, returning `-1`, `0` or `1`. ASCII
letters are lowered before a bytewise comparison, so punctuation between `Z`
and `a` sorts below letters: `"[".casecmp("A")` is `-1`.

### `casecmp?(other: string) -> bool`

Whether two strings are equal under Unicode simple case folding. Expansions
such as `ß` to `ss` are not applied; compare `downcase(:fold)` results for
that. Invalid UTF-8 in either operand falls back to ASCII folding, so distinct
byte sequences stay distinct.

```vibe
"abc".casecmp("ABD")        # -1
"héllo".casecmp?("HÉLLO")   # true
"ß".casecmp?("SS")          # false
```

### `between?(min: string, max: string) -> bool`

Whether the string sorts between `min` and `max`, both included.

### `clamp(min: string?, max: string?) -> string`

The receiver bounded by `min` and `max`; `nil` leaves that side open. `min`
greater than `max` raises.

```vibe
"m".between?("a", "z")  # true
"a".clamp("m", "z")     # "m"
"zz".clamp(nil, "z")    # "z"
```

## Searching

### `include?(text: string) -> bool`

Whether `text` occurs in the string.

### `start_with?(prefix: string, *prefixes: array<string>) -> bool`

Whether the string starts with any of the prefixes.

### `end_with?(suffix: string, *suffixes: array<string>) -> bool`

Whether the string ends with any of the suffixes.

```vibe
"vibescript".include?("script")        # true
"vibescript".start_with?("x", "vibe")  # true
"vibescript".end_with?("vibe")         # false
```

### `index(text: string, offset: int = 0) -> int?`

The character position of the first occurrence of `text` at or after
`offset`, or `nil`. A negative offset counts back from the end; one that lands
before the start gives `nil`. An empty `text` matches at the offset.

### `rindex(text: string, offset?: int) -> int?`

The character position of the last occurrence of `text` starting at or before
`offset`, which defaults to the end. A negative offset counts back from the
end.

```vibe
text = "héllo hello"
text.index("llo")      # 2
text.index("llo", 6)   # 8
text.index("zzz")      # nil
text.rindex("llo")     # 8
text.rindex("llo", 4)  # 2
```

### `slice(start: int | range | string, length?: int) -> string?`

Selects characters, the same as indexing, or a substring:

- an index gives one character; a negative index counts back from the end, and
  an index outside the string gives `nil`;
- an index and a `length` give up to `length` characters; a start exactly at
  the end gives `""`, and a negative length gives `nil`;
- a range gives the characters it selects, with negative bounds counting from
  the end;
- a string gives that string when the receiver contains it, otherwise `nil`.

```vibe
word = "héllo"
word.slice(1)       # "é"
word.slice(-3, 2)   # "ll"
word.slice(1..-1)   # "éllo"
word.slice(1...3)   # "él"
word.slice("llo")   # "llo"
word.slice("x")     # nil
```

### `count(set: string, *sets: array<string>) -> int`

Counts the characters that belong to every given set. A set lists characters
and ranges such as `a-z`; a leading `^` complements it, and `\` escapes `-`,
`^` or `\`.

```vibe
"hello world".count("lo")      # 5
"hello".count("a-y", "^l")     # 3
```

## Regular expressions

A pattern is a regex literal such as `/id-([0-9]+)/i`, a value built with
`Regex.new`, or a string. `match`, `match?` and `scan` read a string pattern as
a regular expression; `sub` and `gsub` match a string pattern literally and
need a regex to match a pattern. Patterns use RE2 syntax, with the `i`
(ignore case) and `m` (`.` matches newlines) flags. Offsets are character
positions.

### `match(pattern: string | regex, offset: int = 0) -> match_data?`

The first match at or after `offset`, or `nil`. A negative offset counts back
from the end, and one past the end still lets an empty match succeed there.
Anchors and `\b` see the characters before the offset. The `match_data`
answers `m[0]` for the whole match and `m[1]`, ... for groups, `captures`,
`named_captures`, `pre_match`, `post_match`, `begin(group)`, `end(group)` and
`to_s`.

```vibe
m = "Order 2024-06-01".match(/(\d+)-(\d+)-(\d+)/)
if m != nil
  year, month, day = m.captures
  m[0]          # "2024-06-01"
  m.pre_match   # "Order "
  m.begin(1)    # 6
end
```

### `match?(pattern: string | regex, offset: int = 0) -> bool`

Whether the pattern matches at or after `offset`, without building match data.
The offset must not be negative; one past the end gives `false`.

```vibe
"Hello".match?(/hello/i)   # true
"abc".match?("b", 2)       # false
```

### `scan(pattern: string | regex) -> array<string | array<string?>>`

Every non-overlapping match. Without groups each element is the matched
string; with groups each element is an array of that match's groups, where a
group that did not take part is `nil`. Narrow the elements to the shape the
pattern gives, for example with a cast.

```vibe
"ID-12 ID-34".scan(/ID-[0-9]+/)       # ["ID-12", "ID-34"]
"a1 b2".scan(/([a-z])([0-9])/)        # [["a", "1"], ["b", "2"]]
"a-b-c".scan(/(\w)(-)?/)              # [["a", "-"], ["b", "-"], ["c", nil]]

ids = "ID-12 ID-34".scan(/ID-[0-9]+/).map { |id| id.as(string) }
```

A scan whose worst-case match table would exceed a fixed 256 MiB host cap is
rejected before it runs; ordinary scans never reach it.

## Replacing

### `sub(pattern: string | regex, replacement: string) -> string` / `sub(pattern: string | regex, &block: string -> string) -> string`

Replaces the first match with `replacement`, or with the block's result for the
matched text. A string pattern matches literally.

### `gsub(pattern: string | regex, replacement: string) -> string` / `gsub(pattern: string | regex, &block: string -> string) -> string`

Replaces every match, the same way.

```vibe
"a.b.c".gsub(".", "-")                     # "a-b-c"
"ID-12 ID-34".sub(/ID-[0-9]+/, "X")        # "X ID-34"
"a1b2".gsub(/([a-z])([0-9])/, "\\2\\1")    # "1a2b"
"hello".gsub("l") { |match| match.upcase } # "heLLo"
```

With a regex pattern, a backslash in `replacement` introduces a reference;
every other character, including `$`, is copied as written. With a string
pattern the replacement is always literal.

| Sequence | Expands to |
| --- | --- |
| `\0`, `\&` | the whole match |
| `\1` to `\9` | that capture group |
| `` \` `` | the text before the match |
| `\'` | the text after the match |
| `\+` | the last group that took part |
| `\k<name>` | the named group `name` |
| `\\` | a backslash |

A backslash before any other character is kept with it. References to groups
that did not take part expand to `""`. Once a pattern names any group, `\1` to
`\9` expand to `""` and named groups are reached with `\k<name>`; a name that
appears twice refers to the last occurrence that took part, and an undefined
name raises.

```vibe
"abc".sub(/b/, "<\\&>")                                            # "a<b>c"
"John Smith".sub(/(?<first>\w+) (?<last>\w+)/, "\\k<last>, \\k<first>") # "Smith, John"
```

### `tr(from: string, to: string) -> string`

Replaces each character of `from` with the character at the same position of
`to`, whose last character repeats as needed. Both take ranges such as `a-z`,
and a `from` starting with `^` translates every character not in it.

### `delete(set: string, *sets: array<string>) -> string`

Removes the characters that belong to every set, with the set syntax of
`count`.

### `squeeze(*sets: array<string>) -> string`

Collapses runs of the same character to one, only for characters in every
given set, or for all characters without a set.

```vibe
"hello".tr("el", "ip")      # "hippo"
"hello".tr("a-y", "b-z")    # "ifmmp"
"hello".delete("l")         # "heo"
"aaabbbccc".squeeze         # "abc"
"aaabbbccc".squeeze("a")    # "abbbccc"
```

### `delete_prefix(prefix: string) -> string`

The string without `prefix` when it starts with it.

### `delete_suffix(suffix: string) -> string`

The string without `suffix` when it ends with it.

```vibe
"unhappy".delete_prefix("un")       # "happy"
"report.csv".delete_suffix(".csv")  # "report"
```

## Case

Case methods use full Unicode case mapping and do not depend on the host's
locale, so a character may expand, as `ß` does to `SS`. Passing `:ascii`
changes only ASCII letters.

### `upcase(mode?: :ascii) -> string`

Converts to uppercase with Unicode case mapping.

### `downcase(mode?: :ascii | :fold) -> string`

Converts to lowercase. `:fold` applies Unicode case folding instead, for
case-insensitive comparison.

### `capitalize(mode?: :ascii) -> string`

Titlecases the first character and lowercases the rest.

### `swapcase(mode?: :ascii) -> string`

Flips the case of every cased character, including circled letters and Roman
numerals. A titlecase digraph such as `ǅ` becomes lowercase.

```vibe
"Straße".upcase           # "STRASSE"
"Straße".upcase(:ascii)   # "STRAßE"
"Straße".downcase(:fold)  # "strasse"
"hÉLLo wORLD".capitalize  # "Héllo world"
"Hello VIBE".swapcase     # "hELLO vibe"
```

### `reverse -> string`

The characters in reverse order.

```vibe
"héllo".reverse  # "olléh"
```

## Whitespace and line endings

`strip`, `lstrip` and `rstrip` remove the ASCII whitespace bytes: space, tab,
newline, vertical tab, form feed, carriage return and NUL. Unicode spaces such
as a non-breaking space are kept.

### `strip -> string`

Removes whitespace from both ends.

### `lstrip -> string`

Removes leading whitespace.

### `rstrip -> string`

Removes trailing whitespace.

### `squish -> string`

Strips both ends and collapses every inner run of whitespace to one space.

```vibe
"  hello  ".strip                 # "hello"
"  hello  ".lstrip                # "hello  "
"  hello  ".rstrip                # "  hello"
"  hello \n\t world  ".squish     # "hello world"
```

### `chomp(separator?: string?) -> string`

Removes one trailing line ending (`"\n"`, `"\r\n"` or `"\r"`), or the given
`separator`. An empty separator removes every trailing `"\r\n"` and `"\n"`, and
`nil` leaves the string unchanged.

### `chop -> string`

Removes the last character, or a trailing `"\r\n"` as a unit.

```vibe
"line\n".chomp        # "line"
"path///".chomp("/")  # "path//"
"line\n\n".chomp("")  # "line"
"héllo".chop          # "héll"
```

## Building strings

### `concat(*texts: array<string>) -> string`

The string followed by each argument in order.

### `prepend(*texts: array<string>) -> string`

The arguments in order, followed by the string.

### `insert(index: int, text: string) -> string`

Inserts `text` before the character at `index`; `index` may equal the length to
append. A negative index inserts after the character it selects, so `-1`
appends. An index outside that range raises.

```vibe
"he".concat("llo", "!")  # "hello!"
"abc".prepend("y", "z")  # "yzabc"
"abc".insert(1, "X")     # "aXbc"
"abc".insert(-2, "X")    # "abXc"
```

### Padding

`width` counts characters. A string already at least `width` long is returned
unchanged. `pad` must not be empty; it repeats and is cut at a character
boundary to fill the space.

### `center(width: int, pad: string = " ") -> string`

Pads both sides; an odd extra character goes on the right.

### `ljust(width: int, pad: string = " ") -> string`

Pads on the right.

### `rjust(width: int, pad: string = " ") -> string`

Pads on the left.

```vibe
"hi".center(6, "-")   # "--hi--"
"hi".center(5)        # " hi  "
"hi".ljust(5, ".")    # "hi..."
"hi".rjust(7, "ab")   # "ababahi"
```

## Splitting

### `split(separator: string? = nil, limit: int = 0) -> array<string>`

Splits the string into fields:

- without a separator, with `nil` or with `" "`, on runs of ASCII whitespace,
  ignoring leading whitespace;
- with `""`, into characters;
- with any other string, on each exact occurrence.

A positive `limit` returns at most that many fields, leaving the rest of the
string in the last one. The default `0` drops trailing empty fields, and a
negative limit keeps them. An empty string gives `[]`.

```vibe
"one two  three".split     # ["one", "two", "three"]
"a,b,".split(",")          # ["a", "b"]
"a,b,".split(",", -1)      # ["a", "b", ""]
"a,b,c,d".split(",", 2)    # ["a", "b,c,d"]
"héllo".split("")          # ["h", "é", "l", "l", "o"]
```

### `partition(separator: string) -> [string, string, string]`

Splits around the first occurrence of `separator`: the text before it, the
separator and the text after it. Without an occurrence the whole string comes
first, followed by two empty strings.

### `rpartition(separator: string) -> [string, string, string]`

Splits around the last occurrence. Without one the whole string comes last,
after two empty strings.

```vibe
key, sep, rest = "name=Ada=admin".partition("=")
key                                 # "name"
rest                                # "Ada=admin"
"name=Ada=admin".rpartition("=")    # ["name=Ada", "=", "admin"]
"no-sep".partition("=")             # ["no-sep", "", ""]
```

### `lines -> array<string>`

The lines, each keeping its trailing `"\n"`. Only `"\n"` ends a line, so a
`"\r"` stays with its line, and a final newline does not add an empty line.

```vibe
"a\nb\n".lines  # ["a\n", "b\n"]
"".lines        # []
```

## Iteration

Each iterator runs its block for every element in order without building an
array first, and returns the receiver.

### `each_char(&block: string) -> string`

Runs the block for each character.

### `each_byte(&block: int) -> string`

Runs the block for each byte, from 0 to 255.

### `each_codepoint(&block: int) -> string`

Runs the block for each code point.

### `each_line(&block: string) -> string`

Runs the block for each line, split as `lines` splits them.

```vibe
initials: array<string> = []
"Ada Lovelace".split.each { |word| initials << word.chr }
initials                          # ["A", "L"]

total = 0
"a\nbb\n".each_line { |line| total += line.chomp.length }
total                             # 3
```

## Templating

### `template(context: hash<string, any>, *, strict: bool = false) -> string`

Replaces each `{{path}}` placeholder with the value at that path in `context`,
where dots step into nested hashes. Placeholders without a value stay as
written, or raise with `strict: true`.

```vibe
page = "Player {{user.name}} scored {{user.score}}"
page.template({ user: { name: "Alex", score: 42 } })  # "Player Alex scored 42"
"Hi {{missing}}".template({ user: "x" })              # "Hi {{missing}}"
```

## Bang variants

These members return the transformed string, or `nil` when the result would
equal the receiver. Strings are values, so the receiver itself never changes:

- `strip!`, `lstrip!`, `rstrip!`, `squish!`, `chomp!`, `chop!`
- `delete!`, `delete_prefix!`, `delete_suffix!`, `tr!`, `squeeze!`
- `upcase!`, `downcase!`, `capitalize!`, `swapcase!`, `reverse!`

`sub!` and `gsub!` return the rewritten string whenever the pattern matched,
even if the replacement reproduces the original text, and `nil` only when it
never matched.

```vibe
"  hello ".strip!          # "hello"
"hello".strip!             # nil
"a".sub!("a", "a")         # "a"
```

## Removed spellings

These names were removed and do not compile; `vibes fix` rewrites them:

- `size` – removed; use `length`, the number of characters.
- `intern` – removed; use `to_sym`.
- `string` – removed; use `to_s`.
- `clear` – removed; write `""`.
- `replace(text)` – removed; use `text` itself.

The `regex:` keyword of `sub`, `gsub`, `sub!` and `gsub!` is removed too: pass a
regex literal or `Regex.new(pattern)` to match a pattern.

## Example: normalizing tags

```vibe
def parse_tags(input: string) -> array<string>
  input
    .downcase
    .split(",")
    .map { |tag| tag.squish }
    .reject { |tag| tag.empty? }
    .uniq
end

parse_tags("  Ruby, Go,,  go , Python  ")  # ["ruby", "go", "python"]
```
