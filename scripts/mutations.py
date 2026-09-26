"""Mutated copies of a program, for the `parse` golden corpus.

Each program yields copies with a token deleted, a token duplicated, a stray
`)`, `]`, `}`, `=`, `,`, `end`, `do`, `|`, `:` or `.` inserted before a token,
and every prefix of whole lines. Tokens are sampled with the caller's random
generator, so a fixed seed gives the same copies.
"""
import re

STRAYS = [")", "]", "}", "=", ",", "end", "do", "|", ":", "."]
TOKEN = re.compile(r'''
    (?P<comment>\#[^\n]*)
  | (?P<ws>[ \t\r]+)
  | (?P<nl>\n)
  | (?P<str>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')
  | (?P<num>\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)
  | (?P<word>@{0,2}[A-Za-z_\u0080-￿][\w\u0080-￿]*[?!]?)
  | (?P<op>\*\*=|\|\|=|&&=|<=>|===|\.\.\.|\*\*|==|!=|<=|>=|&&|\|\||\+=|-=|\*=|/=|%=|<<|::|->|=>|=~|!~|&\.|\.\.)
  | (?P<ch>.)
''', re.X | re.S)


def tokens(source):
    """Returns the (start, end) span of every token outside comments and spaces."""
    return [(m.start(), m.end()) for m in TOKEN.finditer(source) if m.lastgroup not in ("ws", "nl", "comment")]


def mutations(source, rng, per_kind):
    """Yields (kind, text) for each distinct mutation of source."""
    spans = tokens(source)
    seen = set()

    def emit(kind, text):
        if text not in seen and text != source:
            seen.add(text)
            yield kind, text

    if spans:
        def picks():
            return rng.sample(range(len(spans)), min(per_kind, len(spans)))
        for i in picks():
            start, end = spans[i]
            yield from emit("delete", source[:start] + source[end:])
        for i in picks():
            start, end = spans[i]
            yield from emit("duplicate", source[:end] + source[start:end] + source[end:])
        for stray in STRAYS:
            for i in picks():
                start, _ = spans[i]
                gap = " " if stray.isalpha() else ""
                yield from emit("insert " + stray, source[:start] + stray + gap + source[start:])
    lines = source.split("\n")
    for n in range(1, len(lines)):
        yield from emit("truncate", "\n".join(lines[:n]))
