#!/usr/bin/env python3
"""Plot every phase of each independent confirmation, normalized to its control."""
import argparse
import gzip
import json
import pathlib
import statistics

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt


def profile(data, label):
    result = []
    for key, entry in data['variants'][label].items():
        rows = [r for r in data['samples'] if r['round'] >= data['warmup_rounds']
                and r['placement'] == key]
        ratios = [100 * (next(iter(r['times'][label].values())) /
                        next(iter(r['times']['reference'].values())) - 1) for r in rows]
        # Anchored sweeps move the writer; moving sweeps move the island.
        address = data['variants']['string'][key]['address'] if label == 'anchored' else entry['address']
        result.append((address % 128, statistics.mean(ratios)))
    return sorted(result)


def plot(evidence, output):
    with gzip.open(evidence, 'rt') as stream:
        data = json.load(stream)
    fig, axes = plt.subplots(3, 2, figsize=(12, 10), sharex=True, sharey=True)
    titles = {'string': 'Head: move write_string', 'signed': 'Signed: move write_string',
              'prefix': 'Head: move run_prefix', 'run': 'Head: move Output::put_run',
              'anchored': 'Move writer, fixed copy route', 'moving': 'Fixed writer, move copy route'}
    for ax, (label, title) in zip(axes.flat, titles.items()):
        for seed, color in [(223, '#2274a5'), (227, '#db7c26')]:
            points = profile(data['experiments'][f'confirm-{seed}'], label)
            ax.plot(*zip(*points), marker='.', linewidth=1.2, label=f'Seed {seed}', color=color)
        ax.axhline(0, color='#555555', linewidth=0.7)
        ax.set_title(title, fontsize=12)
        ax.grid(alpha=0.2)
        ax.set_xticks(range(0, 129, 16))
        ax.set_xlim(0, 124)
    for ax in axes[:, 0]:
        ax.set_ylabel('Change vs matched reference (%)')
    for ax in axes[-1, :]:
        ax.set_xlabel('Moved code address modulo 128 (bytes)')
    axes[0, 0].legend(loc='upper left', fontsize=9)
    fig.suptitle('Escaped ASCII prose: complete placement cycles on Apple M4', fontsize=16)
    rounds = ' / '.join(str(data['experiments'][f'confirm-{seed}']['rounds']) for seed in [223, 227])
    fig.text(0.5, 0.015, f'All phases retained for plotted runs. Measured cycles: {rounds}, after a full warmup cycle each.\n'
             'Same-binary controls and exclusions are in the raw evidence; see results.md for the recorded run.',
             ha='center', fontsize=10)
    fig.tight_layout(rect=(0, 0.065, 1, 0.96))
    fig.savefig(output, dpi=160)
    plt.close(fig)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    plot(args.evidence, args.output)
