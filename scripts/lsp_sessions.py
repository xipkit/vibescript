"""The `vibes lsp` sessions of the `lsp` golden corpus.

`PROTOCOL` is a session of malformed, unusual and out-of-order messages.
`session` gives one document's session: open, an outline, formatting, hover
and definition at every word, completion after every dot and at every line
start, and signature help after every `(` and `,`; then the same requests
against three unparsable edits; then close.
"""
import re

WORD = re.compile(r'[^\W\d][\w]*[?!]?')


def utf16(text, index):
    return len(text[:index].encode('utf-16-le')) // 2


def message(**fields):
    return {'jsonrpc': '2.0', **fields}


def hover(id, uri, line, character):
    return message(id=id, method='textDocument/hover',
                   params={'textDocument': {'uri': uri}, 'position': {'line': line, 'character': character}})


# Malformed, unusual and out-of-order messages, with how many replies each gets.
PROTOCOL = [
    (b'Content-Length: 9\r\n\r\nnot json!', 0),
    (b'Content-Length: 5\r\n\r\n[1,2]', 0),
    (message(id=1, method=7), 0),
    (message(method='initialize'), 1),
    (message(id='a', method='initialize', params=None), 1),
    (message(id=1.5, method='shutdown'), 1),
    (message(id=None, method='shutdown'), 0),
    (message(method='shutdown'), 0),
    (message(id=2, method='workspace/symbol', params={}), 1),
    (message(method='$/cancelRequest', params={'id': 2}), 0),
    (message(id=3, result=None), 1),
    ({'jsonrpc': '2.0', 'ID': 4, 'METHOD': 'shutdown'}, 1),
    (message(id=5, method='textDocument/hover'), 1),
    (message(id=6, method='textDocument/hover', params=None), 1),
    (message(id=7, method='textDocument/hover',
             params={'textDocument': {'uri': 'file:///p.vibe'}, 'position': {'line': 1.5, 'character': 0}}), 1),
    (hover(8, 'file:///p.vibe', -1, -5), 1),
    (message(id=9, method='textDocument/completion',
             params={'textDocument': {'uri': 'file:///unknown.vibe'}, 'position': {'line': 0, 'character': 0}}), 1),
    (message(method='textDocument/didOpen',
             params={'textDocument': {'uri': 'file:///p.vibe', 'text': 'def run\n  1\nend\n'}}), 1),
    (message(method='textDocument/didOpen',
             params={'textDocument': {'uri': 'file:///p.vibe', 'text': 'def run\n  1\nend\n'}}), 1),
    (message(method='textDocument/didChange',
             params={'textDocument': {'uri': 'file:///p.vibe'}, 'contentChanges': []}), 0),
    (message(method='textDocument/didChange',
             params={'textDocument': {'uri': 'file:///p.vibe'},
                     'contentChanges': [None, {'text': 'def run\n  2\nend\n'}]}), 1),
    (message(method='textDocument/didChange',
             params={'textDocument': {'uri': 'file:///p.vibe'}, 'contentChanges': {'text': 'x'}}), 0),
    (hover(10, 'file:///p.vibe', 0, 5), 1),
    (hover(11, 'file:///p.vibe', 0, 99), 1),
    (hover(12, 'file:///p.vibe', 99, 0), 1),
    (message(id=13, method='textDocument/formatting', params={'textDocument': {'uri': 'file:///missing.vibe'}}), 1),
    (message(id=14, method='textDocument/formatting', params={'textDocument': {'uri': 5}}), 1),
    (message(id=15, method='textDocument/documentSymbol', params={'textDocument': {'uri': 'file:///missing.vibe'}}), 1),
    (message(method='textDocument/didClose', params={'textDocument': {'uri': 'file:///never.vibe'}}), 0),
    (message(method='textDocument/didClose', params={'textDocument': {'uri': 'file:///p.vibe'}}), 1),
    (hover(16, 'file:///p.vibe', 0, 5), 1),
    (message(method='textDocument/didOpen',
             params={'textDocument': {'uri': 'file:///p%20q/caf%C3%A9.vibe', 'text': 'def run(\n'}}), 1),
    (message(id=17, method='textDocument/definition',
             params={'textDocument': {'uri': 'file:///p%20q/caf%C3%A9.vibe'}, 'position': {'line': 0, 'character': 5}}), 1),
    (message(method='textDocument/didClose', params={'textDocument': {'uri': 'file:///p%20q/caf%C3%A9.vibe'}}), 1),
]


def session(uri, text):
    """The messages of one document session, with how many replies each gets.

    The document is opened as written, then replaced by three unparsable
    versions: with a broken definition appended, with that and two comment
    lines inserted at the top, and cut off halfway. Navigation must keep
    working from earlier parses in each, re-anchored to the shifted lines.
    """
    document = {'textDocument': {'uri': uri}}

    def requests(current, full):
        lines = re.split(r'\r\n|\r|\n', current)
        out = [({'method': 'textDocument/documentSymbol', 'params': document}, 1)]
        if full:
            out.append(({'method': 'textDocument/formatting', 'params': document}, 1))
        for number, line in enumerate(lines):
            def at(method, index):
                position = {'line': number, 'character': utf16(line, index)}
                out.append(({'method': method, 'params': {**document, 'position': position}}, 1))
            for word in WORD.finditer(line):
                at('textDocument/definition', word.start())
                at('textDocument/hover', word.start())
                if full:
                    at('textDocument/hover', word.end())
            stripped = len(line) - len(line.lstrip())
            if stripped < len(line):
                at('textDocument/completion', stripped)
            for index, character in enumerate(line):
                if character == '.':
                    at('textDocument/completion', index + 1)
                    end = index + 1
                    while end < len(line) and (line[end].isalnum() or line[end] in '_?!'):
                        end += 1
                    if end > index + 1:
                        at('textDocument/completion', end)
                elif character in '(,':
                    at('textDocument/signatureHelp', index + 1)
        return out

    def change(version, current):
        return ({'method': 'textDocument/didChange',
                 'params': {'textDocument': {'uri': uri, 'version': version},
                            'contentChanges': [{'text': current}]}}, 1)

    messages = [({'method': 'textDocument/didOpen',
                  'params': {'textDocument': {'uri': uri, 'languageId': 'vibescript', 'version': 1,
                                              'text': text}}}, 1)]
    messages += requests(text, True)
    broken = text + ('' if text.endswith('\n') else '\n') + 'def broken(\n'
    shifted = '# shifted\n# twice\n' + broken
    lines = text.splitlines(keepends=True)
    halved = ''.join(lines[:len(lines) // 2])
    for version, current in enumerate([broken, shifted, halved], start=2):
        messages.append(change(version, current))
        messages += requests(current, False)
    messages.append(({'method': 'textDocument/didClose', 'params': document}, 1))
    return messages
