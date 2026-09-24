#!/usr/bin/env python3
"""Record side-by-side `vibes lsp` transcripts of the Go reference and the Rust
port over the site corpus and the reference examples, and compare them.

Each document gets one session per server: open, an outline, formatting,
hover and definition at every word, completion after every dot and at every
line start, and signature help after every `(` and `,`; then the same requests
against three unparsable edits; then close. Both servers are driven in
lockstep, one message at a time, so read-ahead never changes what they see.
Transcripts are written to .cache/lsp-transcripts and a summary to stdout, or
to the file given with --summary.
"""

import argparse
import json
import queue
import re
import subprocess
import sys
import threading
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / '.cache/lsp-transcripts'
REFERENCE = ROOT / '.cache/reference-go-0.70.0'
WORD = re.compile(r'[^\W\d][\w]*[?!]?')


def utf16(text, index):
    return len(text[:index].encode('utf-16-le')) // 2


def corpus():
    documents = sorted((ROOT / 'tests/site').rglob('*.vibe'))
    documents += sorted((REFERENCE / 'examples').rglob('*.vibe'))
    documents += sorted((ROOT / 'tests/lsp').glob('*.vibe'))
    return documents


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


class Server:
    """One language server process driven in lockstep."""

    def __init__(self, command):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE)
        self.replies = queue.Queue()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        stream = self.process.stdout
        while True:
            length = None
            while True:
                line = stream.readline()
                if not line:
                    self.replies.put(None)
                    return
                line = line.strip()
                if not line:
                    break
                name, _, value = line.partition(b':')
                if name.strip().lower() == b'content-length':
                    length = int(value)
            self.replies.put(json.loads(stream.read(length)))

    def send(self, message):
        if isinstance(message, bytes):
            self.process.stdin.write(message)
        else:
            body = json.dumps(message, ensure_ascii=False).encode()
            self.process.stdin.write(b'Content-Length: %d\r\n\r\n' % len(body) + body)
        self.process.stdin.flush()

    def exchange(self, message, expected):
        self.send(message)
        replies = []
        for _ in range(expected):
            try:
                reply = self.replies.get(timeout=120)
            except queue.Empty:
                reply = {'transcript': 'timeout'}
            if reply is None:
                raise RuntimeError('server exited')
            replies.append(reply)
        return replies

    def close(self):
        try:
            self.send({'jsonrpc': '2.0', 'method': 'exit'})
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        self.process.wait(timeout=60)


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


def record(command, documents, path):
    server = Server(command)
    request = 0
    with path.open('w') as out:
        for index, (raw, expected) in enumerate(PROTOCOL):
            replies = server.exchange(raw, expected)
            shown = raw.decode() if isinstance(raw, bytes) else raw
            out.write(json.dumps({'document': 'protocol', 'method': 'protocol', 'position': index,
                                  'message': shown, 'replies': replies}, sort_keys=True) + '\n')
        server.exchange({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {}}, 1)
        server.exchange({'jsonrpc': '2.0', 'method': 'initialized', 'params': {}}, 0)
        for document in documents:
            name = document.relative_to(ROOT).as_posix()
            uri = 'file:///corpus/' + name.replace(' ', '%20')
            text = document.read_text()
            for message, expected in session(uri, text):
                message = {'jsonrpc': '2.0', **message}
                if not message['method'].startswith('textDocument/did'):
                    request += 1
                    message['id'] = request
                replies = server.exchange(message, expected)
                params = message['params']
                out.write(json.dumps({'document': name, 'method': message['method'],
                                      'position': params.get('position'), 'replies': replies},
                                     ensure_ascii=False, sort_keys=True) + '\n')
        server.exchange({'jsonrpc': '2.0', 'id': request + 1, 'method': 'shutdown'}, 1)
    server.close()


PHASES = ['as written', 'broken tail', 'shifted and broken', 'cut in half']


def category(method, go, rust, phase):
    """A coarse reason for a difference, for the summary."""
    if method == 'protocol':
        return 'response'
    if method in ('textDocument/didOpen', 'textDocument/didChange'):
        go_diagnostics = go[0]['params']['diagnostics'] if go else []
        rust_diagnostics = rust[0]['params']['diagnostics'] if rust else []
        if not go_diagnostics and rust_diagnostics:
            return 'checker findings'
        if go_diagnostics and rust_diagnostics:
            return 'compile error presentation'
        return 'other diagnostics'
    return 'response ' + PHASES[phase]


def compare(go_path, rust_path):
    methods = defaultdict(Counter)
    examples = defaultdict(list)
    phases = {}
    strip = lambda replies: [{k: v for k, v in reply.items() if k != 'id'} for reply in replies]
    with go_path.open() as go_lines, rust_path.open() as rust_lines:
        for go_line, rust_line in zip(go_lines, rust_lines, strict=True):
            go, rust = json.loads(go_line), json.loads(rust_line)
            assert (go['document'], go['method'], go['position']) == \
                (rust['document'], rust['method'], rust['position'])
            method, document = go['method'], go['document']
            if method == 'protocol':
                phases[document] = 0
            elif method == 'textDocument/didOpen':
                phases[document] = 0
            elif method == 'textDocument/didChange':
                phases[document] += 1
            if strip(go['replies']) == strip(rust['replies']):
                methods[method]['equal'] += 1
                continue
            reason = category(method, go['replies'], rust['replies'], phases[document])
            methods[method][reason] += 1
            key = (method, reason)
            if len(examples[key]) < 5:
                examples[key].append({'document': document, 'position': go['position'],
                                      'go': go['replies'], 'rust': rust['replies']})
    return methods, examples


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--go', default=str(ROOT / '.cache/cli-lsp/govibes'), help='Go vibes binary')
    parser.add_argument('--rust', default=str(ROOT / 'target/release/vibes'), help='Rust vibes binary')
    parser.add_argument('--summary', help='write the JSON summary here')
    parser.add_argument('--examples', help='write example differences here')
    parser.add_argument('--skip-record', action='store_true', help='compare existing transcripts')
    args = parser.parse_args()
    CACHE.mkdir(parents=True, exist_ok=True)
    go_path, rust_path = CACHE / 'go.jsonl', CACHE / 'rust.jsonl'
    documents = corpus()
    if not args.skip_record:
        if not Path(args.go).exists():
            subprocess.run([str(ROOT / 'scripts/go'), 'build', '-o', args.go, './cmd/vibes'],
                           cwd=REFERENCE, check=True)
        record([args.go, 'lsp'], documents, go_path)
        record([args.rust, 'lsp'], documents, rust_path)
    methods, examples = compare(go_path, rust_path)
    summary = {'documents': len(documents),
               'methods': {method: dict(sorted(counts.items())) for method, counts in sorted(methods.items())}}
    text = json.dumps(summary, indent=2) + '\n'
    if args.summary:
        Path(args.summary).write_text(text)
    sys.stdout.write(text)
    if args.examples:
        flattened = {f'{method} {reason}': cases for (method, reason), cases in sorted(examples.items())}
        Path(args.examples).write_text(json.dumps(flattened, indent=1, ensure_ascii=False) + '\n')


if __name__ == '__main__':
    main()
