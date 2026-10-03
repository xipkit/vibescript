"""Check that the CI driver cannot report a failed checker run as successful."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class CheckerDriverTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(Path(__file__).with_name("fuzz-checker"), self.root / "scripts/fuzz-checker")
        binary = self.root / "target/gate/examples/checker_diff"
        binary.parent.mkdir(parents=True)
        binary.write_text(
            f"#!{sys.executable}\n"
            "import os, sys\n"
            "print(os.environ['TEST_CHECKER_REPORT'])\n"
            "sys.exit(int(os.environ.get('TEST_CHECKER_EXIT', '0')))\n"
        )
        binary.chmod(0o755)

    def run_driver(self, report, exit_code=0):
        return subprocess.run(
            [sys.executable, "scripts/fuzz-checker", "--from", "123", "--count", "1000"],
            cwd=self.root,
            env={**os.environ, "TEST_CHECKER_REPORT": report, "TEST_CHECKER_EXIT": str(exit_code)},
            capture_output=True,
            text=True,
        )

    def test_completed_run_records_replay_command(self):
        result = self.run_driver("generated 1000 accepted 200 findings 0 known 1 in 1.0s")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        log = (self.root / ".cache/fuzz-checker/run.log").read_text()
        self.assertIn("--from 123 --count 1000", log)
        self.assertIn("findings 0 known 1", log)

    def test_findings_fail_even_when_harness_exits_successfully(self):
        result = self.run_driver("generated 1000 accepted 200 findings 1 known 0 in 1.0s")
        self.assertNotEqual(result.returncode, 0)

    def test_incomplete_run_fails_even_without_findings(self):
        result = self.run_driver("generated 999 accepted 200 findings 0 known 0 in 1.0s")
        self.assertNotEqual(result.returncode, 0)

    def test_missing_summary_fails(self):
        self.assertNotEqual(self.run_driver("worker panicked").returncode, 0)

    def test_harness_failure_overrides_clean_summary(self):
        result = self.run_driver("generated 1000 accepted 200 findings 0 known 0 in 1.0s", 3)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
