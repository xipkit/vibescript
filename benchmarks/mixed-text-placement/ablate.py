#!/usr/bin/env python3
"""Toggle only text_span's short-run shortcut in a clean diagnostic source tree."""
import argparse
import pathlib
import subprocess

CANDIDATE = 'd9e3b9e7d79a561df374c3ecac0f3f14919a822a'
PLAIN = '            let n = prefix(rest, class);\n'


def toggle(tree, enabled):
    path = tree / 'src/scan.rs'
    current = path.read_text()
    candidate = subprocess.check_output(['git', 'show', f'{CANDIDATE}:src/scan.rs'], cwd=tree, text=True)
    start = candidate.index('            // One or two bytes,')
    end = candidate.index('            if n == 0', start)
    shortcut = candidate[start:end]
    before, after = (PLAIN, shortcut) if enabled else (shortcut, PLAIN)
    if current.count(before) != 1:
        raise ValueError('expected exactly one text_span shortcut site')
    path.write_text(current.replace(before, after))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tree', type=pathlib.Path)
    parser.add_argument('shortcut', choices=['on', 'off'])
    options = parser.parse_args()
    toggle(options.tree, options.shortcut == 'on')
