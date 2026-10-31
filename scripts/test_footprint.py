"""Check that footprint budget failures expose the captured measurements and panic."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class FootprintDriverTests(unittest.TestCase):
    def test_budget_failure_reports_both_captured_streams(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(Path(__file__).with_name("footprint.py"), root / "scripts/footprint.py")
            cargo = root / "scripts/cargo"
            cargo.write_text(f"#!{sys.executable}\n")
            cargo.chmod(0o755)
            rustc = root / "rustc"
            rustc.write_text(f"#!{sys.executable}\nprint('test compiler')\n")
            rustc.chmod(0o755)
            binary = root / "target/release/examples/footprint"
            binary.parent.mkdir(parents=True)
            binary.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                "print('{\"stage\":\"engine_new\",\"allocations\":42}')\n"
                "if '--check' in sys.argv:\n"
                "    print('panicked: Engine::new allocation count', file=sys.stderr)\n"
                "    sys.exit(101)\n"
            )
            binary.chmod(0o755)
            result = subprocess.run(
                [sys.executable, "scripts/footprint.py", "--out", "report", "--rounds", "1", "--check"],
                cwd=root,
                env={**os.environ, "RUSTC": str(rustc)},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('{"stage":"engine_new","allocations":42}', result.stderr)
            self.assertIn("panicked: Engine::new allocation count", result.stderr)
            self.assertIn("exit status 101", result.stderr)


if __name__ == "__main__":
    unittest.main()
