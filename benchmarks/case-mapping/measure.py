#!/usr/bin/env python3
"""Reproduce VIBE-189 case-mapping inputs and paired macOS measurements."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import statistics
import subprocess

# `compare` warms up with min(iterations, 32) untimed calls. The n and 3n runs
# only get the same warmup when n is at least this.
WARMUP = 32


def workloads():
    prose = ("l'été dernier, nous avons traversé la forêt à pied. près de la rivière, "
             "une lumière très douce éclairait les arbres et les feuilles. l'élève avait "
             "déjà révisé sa leçon, mais personne ne savait où était cachée la clé. "
             "après le dîner, chacun a raconté son voyage et partagé ses idées. ")
    for size in (40 * 1024, 3 * 1024 * 1024):
        rng = random.Random(189)
        pieces, length = [], 0
        while length < size:
            piece = ''.join(rng.choices('abcdefghijklmnopqrstuvwxyz', k=rng.randrange(1, 32))) + 'é'
            pieces.append(piece)
            length += len(piece.encode())
        texts = {'french': (prose * (size // len(prose.encode()) + 1)),
                 'random': ''.join(pieces),
                 'ascii': ('the quick brown fox jumps over the lazy dog. ' * (size // 44 + 1))}
        for family, text in texts.items():
            text = text.encode()[:size].decode(errors='ignore')
            for method in ('upcase', 'downcase'):
                yield dict(name=f'{family}/{size}/{method}', args=[text], accounting=True,
                           source=f'def run(input: string) -> string\ninput.{method}\nend\n',
                           iterations=(5000 if size < 100_000 else 256) if family == 'ascii'
                           else (500 if size < 100_000 else 64))


def run(binary, fixture, count):
    proc = subprocess.run(['/usr/bin/time', '-l', str(binary), str(fixture), str(count), 'timing'],
                          check=True, capture_output=True, text=True)
    row = json.loads(proc.stdout)
    match = re.search(r'(\d+)\s+instructions retired', proc.stderr)
    if not match:
        raise RuntimeError('macOS did not report retired instructions')
    return row, int(match[1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('binaries', nargs='+', help='LABEL=path (baseline first)')
    parser.add_argument('--rounds', type=int, default=5)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error('rounds must be positive')
    short = [case['name'] for case in workloads() if case['iterations'] < WARMUP]
    if short:
        parser.error(f"fewer than {WARMUP} iterations would mix unequal warmups: {', '.join(short)}")
    binaries = dict(item.split('=', 1) for item in args.binaries)
    args.output.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parents[2]
    result = dict(platform=platform.platform(), machine=platform.machine(),
                  cpu=subprocess.check_output(['sysctl', '-n', 'machdep.cpu.brand_string'], text=True).strip(),
                  source_sha256={name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                                 for name in ('src/text/case.rs', 'src/scan.rs')},
                  rustc=subprocess.check_output(['rustc', '-Vv'], text=True),
                  binaries={k: dict(path=v, sha256=hashlib.sha256(Path(v).read_bytes()).hexdigest())
                            for k, v in binaries.items()}, cases=[])
    rng = random.Random(189)
    for case in workloads():
        fixture = args.output / 'case.json'
        fixture.write_text(json.dumps([case], ensure_ascii=False) + '\n')
        record = dict(name=case['name'], bytes=len(case['args'][0].encode()),
                      fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(), samples=[])
        signature = None
        for repeat in range(args.rounds):
            order = list(binaries)
            rng.shuffle(order)
            for label in order:
                n = case['iterations']
                rows = {}
                for count in ([n, 3*n] if repeat % 2 == 0 else [3*n, n]):
                    row, instructions = run(binaries[label], fixture, count)
                    current = {k: row[k] for k in ('digest', 'steps', 'tracked_peak_bytes', 'tracked_retained_bytes')}
                    if signature is None:
                        signature = current
                    if current != signature:
                        raise RuntimeError(f"output/accounting mismatch: {case['name']} {label}")
                    rows[count] = dict(ns=row['ns_per_call'], instructions=instructions)
                record['samples'].append(dict(label=label, repeat=repeat, iterations=n, runs=rows, loads=os.getloadavg(),
                    ns=rows[3*n]['ns'], instructions=(rows[3*n]['instructions']-rows[n]['instructions'])/(2*n)))
        record['signature'] = signature
        record['medians'] = {label: {metric: statistics.median(s[metric] for s in record['samples'] if s['label'] == label)
                                   for metric in ('ns', 'instructions')} for label in binaries}
        result['cases'].append(record)
        (args.output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        print(case['name'], record['medians'], flush=True)
    fixture.unlink()


if __name__ == '__main__':
    main()
