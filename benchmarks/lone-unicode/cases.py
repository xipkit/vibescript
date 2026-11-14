#!/usr/bin/env python3
"""Deterministic VIBE-188 fixtures, with independently computed lengths."""
import json
import pathlib
import random
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / 'scripts'))
from fixtures import function, text_benchmark_cases


def mixed(size, seed=188, character=None):
    rng = random.Random(seed)
    parts, length = [], 0
    while length < size:
        word = ''.join(rng.choice('abcdefghijklmnopqrstuvwxyz') for _ in range(rng.randint(1, 15)))
        separator = rng.choice('é界🙂')
        part = word + (character or separator)
        parts.append(part)
        length += len(part.encode())
    return ''.join(parts).encode()[:size].decode('utf-8', errors='ignore')


def cases():
    out = []
    def add(name, text, iterations):
        out.append(dict(name=name, source=function('input.length', 'int', 'string'),
                        args=[text], expected=len(text), accounting=True, iterations=iterations))
    for size in [40_000, 3_000_000]:
        text = mixed(size)
        add(f'mixed_{size}', text, 3000 if size == 40_000 else 160)
    for name, phrase in [
        ('french', 'Après une journée passée à étudier, elle préfère écouter les récits de ses amis. '),
        ('russian', 'После долгого дня мы идём домой и обсуждаем интересные истории наших друзей. '),
        ('japanese', '長い一日の後で私たちは家に帰り、友人たちの面白い物語について話し合います。 '),
    ]:
        text = (phrase * (40_000 // len(phrase.encode()) + 1)).encode()[:40_000].decode('utf-8', errors='ignore')
        add(name, text, 3000)
    add('ascii_40000', 'a' * 40_000, 15000)
    for case in text_benchmark_cases():
        if case['name'] == 'text/length_unicode/metered':
            out.append(dict(case, iterations=3000))
    obj = dict(id=7, payload='é界🙂' * 456)
    raw = json.dumps(obj, ensure_ascii=False, separators=(',', ':'), sort_keys=True)
    out.append(dict(name='json_parse_unicode_4k', source=function('JSON.parse(input)', 'any', 'string'),
                    args=[raw], expected=obj, accounting=True, iterations=15000))
    return out


if __name__ == '__main__':
    pathlib.Path(sys.argv[1]).write_text(json.dumps(cases(), ensure_ascii=False) + '\n')
