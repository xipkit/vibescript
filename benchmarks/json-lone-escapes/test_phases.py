"""Check that differently located functions are compared at matching PC phases."""
import json
from pathlib import Path
import tempfile
import unittest

from phases import Variant, manifests, phases


def cohort(start: int) -> list[Variant]:
    """Construct one relinker cycle with an arbitrary initial PC phase."""
    return [dict(path=f'pad-{i:02}', symbol='selected', address=start + 4 * i,
                 pad_bytes=4 * (i + 1), sha256='test') for i in range(32)]


class PhaseTests(unittest.TestCase):
    """PC phase, not the order-file pad, defines a matched comparison."""

    def test_different_pad_indices_match_actual_addresses(self) -> None:
        """Each before/head pair must have the same address modulo 128."""
        before = phases(cohort(0x1000 + 20), 0, 0x8000, 'string')
        after = phases(cohort(0x4000 + 68), 0, 0x9000, 'string')
        self.assertEqual(before.keys(), after.keys())
        for key in before:
            self.assertEqual(before[key]['address'] % 128, after[key]['address'] % 128)
            self.assertEqual(before[key]['writer_address'], before[key]['address'])
            self.assertEqual(before[key]['helper_address'], 0x8000)
        self.assertNotEqual(before['local/00']['path'], after['local/00']['path'])

    def test_helper_sweep_keeps_the_writer_fixed(self) -> None:
        """A helper sweep changes only the helper's recorded PC."""
        variants = phases(cohort(0x1000 + 12), 0x8040, 0, 'run')
        self.assertEqual({entry['writer_address'] for entry in variants.values()}, {0x8040})
        self.assertEqual({entry['helper_address'] % 128 for entry in variants.values()},
                         set(range(0, 128, 4)))

    def test_incomplete_duplicate_and_unaligned_cycles_are_rejected(self) -> None:
        """A malformed cohort must not produce a complete-looking distribution."""
        for variants in [cohort(0x1000)[:-1], [*cohort(0x1000)[:-1], cohort(0x1000)[0]],
                         cohort(0x1001)]:
            with self.subTest(variants=variants), self.assertRaises(ValueError):
                phases(variants, 0, 0, 'string')

    def test_manifests_use_each_sweeps_fixed_symbol_addresses(self) -> None:
        """Initially reordering a different function changes the fixed addresses."""
        with tempfile.TemporaryDirectory() as temporary:
            build = Path(temporary)
            for mode in ('simd', 'portable'):
                for label in ('base', 'head'):
                    for target, start in [('string', 0x1014), ('run', 0x400c)]:
                        directory = build / f'{mode}-{label}/{target}-placements'
                        directory.mkdir(parents=True)
                        variants = cohort(start)
                        for entry in variants:
                            entry['symbol'] = target
                        symbols = {'string': [start if target == 'string' else 0xb040, 't'],
                                   'run': [start if target == 'run' else 0xa040, 't']}
                        (directory / 'variants.json').write_text(json.dumps(dict(enumerate(variants))))
                        (directory / 'symbols.json').write_text(json.dumps(symbols))
            manifests(build)
            groups = json.loads((build / 'groups.json').read_text())
            self.assertEqual(len(groups), 8)
            for name, variants in groups.items():
                self.assertEqual(len(variants), 32)
                if name.endswith('string'):
                    self.assertEqual({v['helper_address'] for v in variants.values()}, {0xa040})
                else:
                    self.assertEqual({v['writer_address'] for v in variants.values()}, {0xb040})


if __name__ == '__main__':
    unittest.main()
