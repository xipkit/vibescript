#!/usr/bin/env python3
"""Summarize placement distributions without selecting a favorable address."""
import argparse
import collections
import json
import statistics


def summarize(data):
    labels = [*data['variants'], 'control']
    baseline = labels[0]
    names = list(data['signatures'])
    for mode in ['local', 'front']:
        records = [row for row in data['samples'] if row['placement'].startswith(mode + '/')
                   and row['round'] >= data.get('warmup_rounds', 0)]
        if not records:
            continue
        print(f'\n{mode}: {len(records)} matched groups; time is median of placement minima (range), microseconds')
        for name in names:
            print(name)
            for label in labels:
                placements = collections.defaultdict(list)
                ratios = []
                for row in records:
                    value = row['times'][label][name]
                    placements[row['placement']].append(value)
                    ratios.append(value / row['times'][baseline][name])
                minima = [min(values) / 1000 for values in placements.values()]
                print(f'  {label:12} {statistics.median(minima):8.3f}'
                      f' ({min(minima):.3f}..{max(minima):.3f})'
                      f' paired {100 * (statistics.median(ratios) - 1):+.2f}%')
            for before, after in [('base', 'base-short'), ('no-short', 'candidate'),
                                  ('base', 'no-short'), ('base-short', 'candidate')]:
                if before in labels and after in labels:
                    ratios = [r['times'][after][name] / r['times'][before][name] for r in records]
                    print(f'    {before} -> {after}: {100 * (statistics.median(ratios) - 1):+.2f}% paired')
    print(f"\n{len(data['rejected'])} rejected groups; accounting matched for {len(names)} cases")
    print('peak accepted load:', max(max(row['loads']) for row in data['samples']))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('samples')
    options = parser.parse_args()
    with open(options.samples) as stream:
        summarize(json.load(stream))
