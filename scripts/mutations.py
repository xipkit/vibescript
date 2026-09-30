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


def recovery_mutations(source, rng):
    """Combines distant edits and malformed regions for recovery stress tests."""
    spans = tokens(source)
    for count in (2, 4, 8):
        positions = sorted(rng.sample(spans, min(count, len(spans))), reverse=True)
        text = source
        for start, end in positions:
            text = text[:start] + rng.choice(STRAYS) + text[end:]
        yield f"combined-{count}", text
    yield "independent-lines", source + "\nfirst = )\nsecond = ]\nthird = }\n"
    yield "broken-declarations", "def broken(,\nend\n" + source + "\nclass\nend\n"


SUFFIX_RUNS = ["?", "!", "??", "?!", "!?"]


def suffix_mutations(source, rng, per_run=3):
    """Appends each run of `?` and `!` to a few sampled names, so the sweep
    meets suffixed bindings, reads and definitions everywhere a name stands.
    Each class, module, enum and type alias name also takes each run, once
    at its declarations alone and once at every use."""
    words = [(m.start(), m.end()) for m in TOKEN.finditer(source) if m.lastgroup == "word"]
    for run in SUFFIX_RUNS:
        for index in sorted(rng.sample(range(len(words)), min(per_run, len(words)))):
            _, end = words[index]
            yield f"suffix {run}:{index}", source[:end] + run + source[end:]
    spelled = [source[start:end] for start, end in words]
    declared = {}
    for index, word in enumerate(spelled[:-1]):
        name = spelled[index + 1]
        if word in ("class", "module", "enum", "type") and name[0].isupper() and name[-1] not in "?!":
            declared.setdefault(name, []).append(index + 1)
    for name, declarations in declared.items():
        uses = [index for index, word in enumerate(spelled) if word == name]
        for kind, indexes in (("declaration", declarations), ("uses", uses)):
            if kind == "uses" and uses == declarations:
                continue
            for run in SUFFIX_RUNS:
                text = source
                for index in reversed(indexes):
                    _, end = words[index]
                    text = text[:end] + run + text[end:]
                yield f"suffix {kind} {name}{run}", text


def group_mutations(source, rng, per_kind=3):
    """Wraps a few call receivers in parentheses, once and twice, as in
    `(JSON).parse_as(...)`, which the compiler's parser reads as the bare
    receiver, so the rules' parser must read them alike."""
    spans = [(m.start(), m.end(), m.lastgroup, m.group()) for m in TOKEN.finditer(source)
             if m.lastgroup not in ("ws", "nl", "comment")]
    receivers = []
    for index in range(len(spans) - 2):
        start, end, kind, text = spans[index]
        previous = spans[index - 1][3] if index else ""
        if kind == "word" and spans[index + 1][3] in (".", "&.") \
                and spans[index + 1][0] == end and spans[index + 2][2] == "word" \
                and previous not in (".", "&.", "::", "def"):
            receivers.append((start, end))
    for depth in (1, 2):
        for index in sorted(rng.sample(range(len(receivers)), min(per_kind, len(receivers)))):
            start, end = receivers[index]
            text = "(" * depth + source[start:end] + ")" * depth
            yield f"group {depth}:{index}", source[:start] + text + source[end:]
