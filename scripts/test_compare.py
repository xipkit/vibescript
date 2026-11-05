"""Check timing reliability against saved samples and measurement output."""

import contextlib
import io
import json
from pathlib import Path
import statistics
import tempfile
import unittest
from unittest.mock import patch

import compare


def summary_for(samples, variants=("rust-portable", "rust-simd")):
    return {
        "rounds": len(samples[0]),
        "cases": {
            f"case-{index}": {
                variant: {"samples_ns": times[:], "median_ns": statistics.median(times)}
                for variant in variants
            }
            for index, times in enumerate(samples)
        },
    }


class TimingReliabilityTests(unittest.TestCase):
    def test_saved_summary_includes_baseline_binaries(self):
        path = compare.ROOT / "benchmarks/results/json-simd-followup/arm64-core-summary.json"
        summary = json.loads(path.read_text())
        reports = compare.timing_reliability(summary)
        self.assertEqual(len(reports), 4)
        for report in reports.values():
            self.assertEqual(report, {
                "disturbed_rounds": [], "pulled_up_medians": 0,
                "case_count": 74, "reliable": True,
            })

    def test_sample_thresholds_are_strict(self):
        summary = summary_for([[100, 120, 120, 120]])
        report = compare.timing_reliability(summary)["rust-portable"]
        self.assertEqual(report["pulled_up_medians"], 0)
        self.assertTrue(report["reliable"])
        summary = summary_for([[100, 140, 140.1, 100]])
        report = compare.timing_reliability(summary)["rust-portable"]
        self.assertEqual(report["disturbed_rounds"], [3])

    def test_five_percent_is_allowed_but_more_warns_per_binary(self):
        summary = summary_for([[100] * 4 for _ in range(20)])
        summary["cases"]["case-0"]["rust-portable"] = {
            "samples_ns": [100, 121, 121, 121], "median_ns": 121,
        }
        report = compare.timing_reliability(summary)["rust-portable"]
        self.assertEqual(report["pulled_up_medians"], 1)
        self.assertTrue(report["reliable"])
        summary["cases"]["case-1"]["rust-portable"] = {
            "samples_ns": [100, 121, 121, 121], "median_ns": 121,
        }
        reports = compare.timing_reliability(summary)
        self.assertEqual(reports["rust-portable"]["pulled_up_medians"], 2)
        self.assertFalse(reports["rust-portable"]["reliable"])
        self.assertTrue(reports["rust-simd"]["reliable"])

    def test_disturbed_round_requires_more_than_half_the_cases(self):
        summary = summary_for([[200, 100, 100, 100], [100] * 4])
        self.assertEqual(compare.timing_reliability(summary)["rust-portable"]["disturbed_rounds"], [])
        summary = summary_for([[200, 100, 100, 100]] * 2)
        report = compare.timing_reliability(summary)["rust-portable"]
        self.assertEqual(report["disturbed_rounds"], [1])
        self.assertTrue(report["reliable"])

    def test_half_the_rounds_disturbed_is_unreliable_even_with_stable_medians(self):
        samples = [[100] * 8 for _ in range(20)]
        for round_index in range(4):
            for offset in range(11):
                samples[(5 * round_index + offset) % 20][round_index] = 200
        report = compare.timing_reliability(summary_for(samples))["rust-portable"]
        self.assertEqual(report["disturbed_rounds"], [1, 2, 3, 4])
        self.assertEqual(report["pulled_up_medians"], 0)
        self.assertFalse(report["reliable"])


class MeasurementReportTests(unittest.TestCase):
    def test_measure_saves_and_prints_each_binary_and_warns_only_when_unreliable(self):
        for noisy in (False, True):
            with self.subTest(noisy=noisy), tempfile.TemporaryDirectory() as directory:
                out = Path(directory)
                cases = [{"name": f"case-{index}"} for index in range(20)]

                def invoke(variant, fixtures, iterations, mode, path):
                    records = []
                    for index, case in enumerate(cases):
                        timing = 100
                        if noisy and variant == "rust-portable" and path.name.startswith("round-"):
                            round_index = int(path.name.split("-")[1])
                            if round_index == 1 or (index < 2 and round_index in (2, 3, 4)):
                                timing = 200
                        records.append({
                            "name": case["name"], "digest": "digest", "ns_per_call": timing,
                            "alloc_bytes": 0, "allocations": 0, "steps": 1,
                            "tracked_peak_bytes": 0, "tracked_retained_bytes": 0,
                        })
                    return records

                def rss_run(cmd, **kwargs):
                    kwargs["stderr"].write("1024 maximum resident set size\n")

                stdout, stderr = io.StringIO(), io.StringIO()
                with patch.object(compare, "VARIANTS", ["rust-portable", "rust-simd"]), \
                     patch.object(compare, "benchmark_cases", return_value=cases), \
                     patch.object(compare, "invoke", side_effect=invoke), \
                     patch.object(compare, "run", side_effect=rss_run), \
                     contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    compare.measure(out, 8, 1, {case["name"]: "digest" for case in cases}, "core")

                summary = json.loads((out / "summary.json").read_text())
                portable = summary["timing_reliability"]["rust-portable"]
                self.assertEqual(portable, {
                    "disturbed_rounds": [2] if noisy else [],
                    "pulled_up_medians": 2 if noisy else 0,
                    "case_count": 20, "reliable": not noisy,
                })
                self.assertEqual(summary["cases"]["case-0"]["rust-portable"]["samples_ns"],
                                 [100, 200, 200, 200, 200, 100, 100, 100] if noisy else [100] * 8)
                self.assertIn(f"rust-portable: disturbed rounds {portable['disturbed_rounds']} of 8; "
                              f"medians >1.2x own best: {portable['pulled_up_medians']}/20; "
                              f"{'UNRELIABLE' if noisy else 'ok'}", stdout.getvalue())
                self.assertIn("rust-simd: disturbed rounds [] of 8; medians >1.2x own best: 0/20; ok", stdout.getvalue())
                if noisy:
                    self.assertIn("WARNING: rust-portable timing is UNRELIABLE", stderr.getvalue())
                    self.assertNotIn("rust-simd", stderr.getvalue())
                else:
                    self.assertEqual(stderr.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
