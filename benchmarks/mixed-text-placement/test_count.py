"""Check that count.py's differential estimate cancels compare's warmup calls."""
import json
import pathlib
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import count

ROOT = pathlib.Path(__file__).resolve().parents[2]
SETUP = 5_000_000
PER_CALL = 1_000


def fake_time(command, **_):
    """Stand in for `/usr/bin/time -l compare FIXTURE 0 timing`."""
    [case] = json.loads(pathlib.Path(command[3]).read_text())
    timed = case['iterations']
    calls = timed + min(timed, count.WARMUP)
    stderr = f'  {SETUP + PER_CALL * calls}  instructions retired\n'
    return subprocess.CompletedProcess(command, 0, stdout='', stderr=stderr)


class CountTests(unittest.TestCase):
    def test_warmup_matches_compare(self):
        source = (ROOT / 'examples/compare.rs').read_text()
        warmups = set(re.findall(r'n\.min\((\d+)\)', source))
        self.assertEqual(warmups, {str(count.WARMUP)})

    def test_estimate_excludes_warmup_calls(self):
        for iterations, calls in [(n, calls) for n in (1, 40, 80, 127, 128, 160, 4_800, 32_000)
                                  for calls in (None, 32, 3000)]:
            with self.subTest(iterations=iterations, calls=calls), tempfile.TemporaryDirectory() as directory:
                directory = pathlib.Path(directory)
                fixtures = directory / 'cases.json'
                fixtures.write_text(json.dumps([{'name': 'case', 'iterations': iterations}]))
                output = directory / 'counts.json'
                with patch.object(count.subprocess, 'run', side_effect=fake_time), \
                        patch('builtins.print'):
                    count.count(fixtures, {'binary': 'compare'}, output, runs=2, calls=calls)
                for result in json.loads(output.read_text()):
                    self.assertGreaterEqual(result['n'], count.WARMUP)
                    if calls is not None:
                        self.assertEqual(result['n'], calls)
                    self.assertEqual(result['instructions_per_call'], PER_CALL)

    def test_explicit_calls_cannot_change_the_warmup_count(self):
        with self.assertRaisesRegex(ValueError, 'calls must be at least 32'):
            count.count(pathlib.Path('unused'), {}, pathlib.Path('unused'), runs=1, calls=31)


if __name__ == '__main__':
    unittest.main()
