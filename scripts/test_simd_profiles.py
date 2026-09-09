import json
from pathlib import Path
import tempfile
import unittest

import simd_profiles


class SIMDProfilesTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.head = self.root / "head"
        self.base = self.root / "base"
        self.fixture = "internal/runtime/small_benchmark_test.go"
        self.source = "internal/runtime/small.go"
        self.profile = "benchmarks/simd/small.json"
        for root in [self.head, self.base]:
            self.write(root, self.profile, json.dumps({
                "fixture": self.fixture,
                "benchmark": "BenchmarkSmall",
                "cases": 2,
                "sources": ["internal/runtime/small*.go"],
            }))
            self.write(root, self.fixture, "standalone benchmark fixture\n")
            self.write(root, self.source, "old production code\n")

    def write(self, root, relative, content):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)

    def test_no_base_runs_only_controls(self):
        plan = simd_profiles.prepare(self.head, None)
        self.assertEqual(plan["groups"], [simd_profiles.CONTROLS])
        self.assertEqual(plan["cases"], 8)

    def test_unchanged_profile_is_inactive(self):
        self.write(self.head, "unrelated.go", "new unrelated code\n")
        self.write(self.head, "internal/runtime/small_test.go", "new test\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 8)

    def test_missing_fixture_is_inactive(self):
        (self.head / self.fixture).unlink()
        self.write(self.head, self.source, "changed source\n")
        self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 8)

    def test_profile_fixture_and_source_changes_activate(self):
        for relative in [self.profile, self.fixture, self.source]:
            with self.subTest(relative=relative):
                before = (self.head / relative).read_text()
                self.write(self.head, relative, before + "\n")
                self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 10)
                self.write(self.head, relative, before)
                self.write(self.base, relative, before)

    def test_source_glob_additions_and_deletions_activate(self):
        added = "internal/runtime/small_simd.go"
        self.write(self.head, added, "new implementation\n")
        self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 10)
        (self.head / added).unlink()
        (self.head / self.source).unlink()
        self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 10)

    def test_deleted_profile_uses_its_base_definition(self):
        (self.head / self.profile).unlink()
        self.write(self.head, self.source, "changed production code\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertEqual(plan["groups"][1]["name"], "small")
        self.assertFalse((self.head / self.profile).exists())

    def test_deleted_profile_without_head_fixture_is_inactive(self):
        (self.head / self.profile).unlink()
        (self.head / self.fixture).unlink()
        self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 8)

    def test_renamed_profile_is_selected_once(self):
        (self.head / self.profile).rename(self.head / "benchmarks/simd/renamed.json")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertEqual(plan["groups"][1]["name"], "renamed")

    def test_copy_is_limited_to_selected_fixtures(self):
        self.write(self.head, self.fixture, "updated standalone fixture\n")
        self.write(self.head, self.source, "updated production code\n")
        self.write(self.head, "internal/runtime/unselected_benchmark_test.go", "do not copy\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["groups"][1]["name"], "small")
        self.assertEqual((self.base / self.fixture).read_bytes(), (self.head / self.fixture).read_bytes())
        self.assertEqual((self.base / self.source).read_text(), "old production code\n")
        self.assertFalse((self.base / "internal/runtime/unselected_benchmark_test.go").exists())

    def test_fixture_cannot_escape_checkout(self):
        profile = json.loads((self.head / self.profile).read_text())
        profile["fixture"] = "internal/runtime/../../outside_benchmark_test.go"
        self.write(self.head, self.profile, json.dumps(profile))
        with self.assertRaisesRegex(ValueError, "invalid repository path"):
            simd_profiles.prepare(self.head, self.base)

    def test_profile_metadata_is_checked_before_fixture_arrives(self):
        (self.head / self.fixture).unlink()
        profile = json.loads((self.head / self.profile).read_text())
        profile["cases"] = 0
        self.write(self.head, self.profile, json.dumps(profile))
        with self.assertRaisesRegex(ValueError, "invalid case count"):
            simd_profiles.prepare(self.head, self.base)

    def results(self):
        self.write(self.head, self.source, "changed source\n")
        plan = simd_profiles.prepare(self.head, self.base)
        names = [
            f"BenchmarkString{operation}Loop{kind}"
            for operation in ["Length", "Index", "RIndex", "Slice"]
            for kind in ["ASCII", "Unicode"]
        ] + ["BenchmarkSmall/first", "BenchmarkSmall/second"]
        lines = [f"{name} 100 12.3 ns/op 0 B/op 0 allocs/op\n" for name in names for _ in range(6)]
        results = self.root / "results"
        for variant in ["head-nosimd", "head-simd", "base-nosimd", "base-simd", "head-simd-avx2-disabled", "base-simd-avx2-disabled"]:
            self.write(results, variant + ".txt", "".join(lines))
        return plan, results

    def test_result_validation_accepts_complete_variants(self):
        plan, results = self.results()
        simd_profiles.validate(plan, results, avx2_disabled=True)

    def test_disabled_comparison_requires_the_pr_base(self):
        plan, results = self.results()
        (results / "base-simd-avx2-disabled.txt").unlink()
        with self.assertRaises(FileNotFoundError):
            simd_profiles.validate(plan, results, avx2_disabled=True)

    def test_disabled_run_without_base_requires_only_head(self):
        plan, results = self.results()
        plan["base"] = False
        for path in results.glob("base-*.txt"):
            path.unlink()
        simd_profiles.validate(plan, results, avx2_disabled=True)

    def test_duplicate_samples_cannot_hide_missing_case(self):
        plan, results = self.results()
        path = results / "head-simd.txt"
        path.write_text(path.read_text().replace("BenchmarkSmall/second", "BenchmarkSmall/first"))
        with self.assertRaisesRegex(ValueError, "unique cases with six samples"):
            simd_profiles.validate(plan, results)

    def test_unselected_case_cannot_replace_selected_case(self):
        plan, results = self.results()
        path = results / "head-simd.txt"
        path.write_text(path.read_text().replace("BenchmarkSmall/second", "BenchmarkUnselected/second"))
        with self.assertRaisesRegex(ValueError, "exactly one selected profile"):
            simd_profiles.validate(plan, results)

    def test_case_names_must_match_across_variants(self):
        plan, results = self.results()
        path = results / "base-simd.txt"
        path.write_text(path.read_text().replace("BenchmarkSmall/second", "BenchmarkSmall/different"))
        with self.assertRaisesRegex(ValueError, "case names differ"):
            simd_profiles.validate(plan, results)

    def test_each_profile_has_its_own_case_count(self):
        plan, results = self.results()
        for path in results.glob("*.txt"):
            path.write_text(path.read_text().replace("BenchmarkSmall/second", "BenchmarkStringLengthLoopASCII/extra"))
        with self.assertRaisesRegex(ValueError, "incorrect case count"):
            simd_profiles.validate(plan, results)


if __name__ == "__main__":
    unittest.main()
