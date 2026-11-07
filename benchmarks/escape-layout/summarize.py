#!/usr/bin/env python3
"""Report all phases; never select a pad as an optimization."""
import argparse
import collections
import json
import math
import pathlib
import statistics


def summarize(data):
    placements = set(next(iter(data['variants'].values())))
    expected = {(r, p) for r in range(data['rounds'] + data['warmup_rounds']) for p in placements}
    actual = [(r['round'], r['placement']) for r in data['samples']]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError('incomplete or duplicate cycle')
    baseline = next(iter(data['variants']))
    by_phase = collections.defaultdict(lambda: collections.defaultdict(list))
    phase_ratios = collections.defaultdict(lambda: collections.defaultdict(list))
    ratios = collections.defaultdict(list)
    for row in data['samples']:
        if max(row['loads']) >= data['load_limit']:
            raise ValueError('accepted group violates load cutoff')
        if row['round'] < data['warmup_rounds']:
            continue
        for label, cases in row['times'].items():
            for case, value in cases.items():
                by_phase[(row['placement'], case)][label].append(value / 1000)
                ratio = 100 * (value / row['times'][baseline][case] - 1)
                ratios[(case, label)].append(ratio)
                phase_ratios[(row['placement'], case)][label].append(ratio)
    phases = []
    for (placement, case), groups in sorted(by_phase.items()):
        phases.append({'placement': placement, 'case': case,
                       'mean_us': {label: statistics.mean(values) for label, values in groups.items()},
                       'mean_paired_percent': {label: statistics.mean(values)
                           for label, values in phase_ratios[(placement, case)].items()}})
    harmonic = {}
    for case in data['signatures']:
        selected = [p for p in phases if p['case'] == case]
        count = len(selected)
        harmonic[case] = {}
        for label in selected[0]['mean_paired_percent']:
            values = [p['mean_paired_percent'][label] for p in selected]
            cosine = 2 / count * sum(v * math.cos(2 * math.pi * i / count) for i, v in enumerate(values))
            sine = 2 / count * sum(v * math.sin(2 * math.pi * i / count) for i, v in enumerate(values))
            harmonic[case][label] = 2 * math.hypot(cosine, sine)
    return {'seed': data['seed'], 'rejected': len(data['rejected']), 'phases': phases,
            'harmonic_peak_to_trough_percent': harmonic,
            'mean_paired_percent': {case: {label: statistics.mean(values)
                for (name, label), values in ratios.items() if name == case}
                for case in data['signatures']}}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=pathlib.Path)
    args = parser.parse_args()
    print(json.dumps(summarize(json.loads(args.input.read_text())), indent=2))
