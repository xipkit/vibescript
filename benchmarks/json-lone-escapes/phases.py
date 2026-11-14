"""Match placement cohorts by actual function-address phase rather than pad index."""
from __future__ import annotations

import argparse
from collections.abc import Iterable
import json
from pathlib import Path
from typing import TypedDict


class Variant(TypedDict):
    """An executable emitted by the isolated-function relinker."""

    path: str
    symbol: str
    address: int
    pad_bytes: int
    sha256: str


class PhaseVariant(Variant):
    """A placement with both hot functions' addresses recorded."""

    writer_address: int
    helper_address: int


def phases(variants: Iterable[Variant], writer_address: int, helper_address: int,
           target: str) -> dict[str, PhaseVariant]:
    """Require a complete 128-byte cycle and key it by the selected function's PC."""
    if target not in ('string', 'run'):
        raise ValueError(f'unknown placement target: {target}')
    result: dict[str, PhaseVariant] = {}
    for variant in variants:
        address = variant['address']
        if address % 4:
            raise ValueError('unaligned ARM64 function address')
        key = f'local/{address % 128 // 4:02}'
        if key in result:
            raise ValueError(f'duplicate function-address phase: {key}')
        result[key] = {
            **variant,
            'writer_address': address if target == 'string' else writer_address,
            'helper_address': address if target == 'run' else helper_address,
        }
    if len(result) != 32:
        raise ValueError('expected all 32 function-address phases')
    return dict(sorted(result.items()))


def manifests(build: Path) -> None:
    """Write phase-matched manifests for independent writer and helper sweeps."""
    groups: dict[str, dict[str, PhaseVariant]] = {}
    for mode in ('simd', 'portable'):
        for label in ('base', 'head'):
            directory = build / f'{mode}-{label}'
            variants: dict[str, dict[str, Variant]] = {
                target: json.loads((directory / f'{target}-placements/variants.json').read_text())
                for target in ('string', 'run')
            }
            if any(len(entries) != 32 for entries in variants.values()):
                raise ValueError('expected 32 relinked executables per target')
            selected = {target: next(iter(entries.values()))['symbol']
                        for target, entries in variants.items()}
            for target, entries in variants.items():
                symbols = json.loads((directory / f'{target}-placements/symbols.json').read_text())
                cohort = phases(entries.values(), symbols[selected['string']][0],
                                symbols[selected['run']][0], target)
                name = f'{mode}-{label}-{target}'
                groups[name] = cohort
                (build / f'{name}.json').write_text(json.dumps(cohort, indent=2) + '\n')
    (build / 'groups.json').write_text(json.dumps(groups, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=Path)
    options = parser.parse_args()
    manifests(options.build.resolve())
