import hashlib
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
            for relative in simd_profiles.CONTROLS["inputs"]:
                self.write(root, relative, "shared benchmark helper\n")
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
        self.assertEqual(len(plan["groups"]), 1)
        self.assertEqual(plan["groups"][0]["name"], "controls")
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

    def test_renamed_fixture_replaces_old_base_copy(self):
        renamed = "internal/runtime/renamed_benchmark_test.go"
        (self.head / self.fixture).rename(self.head / renamed)
        profile = json.loads((self.head / self.profile).read_text())
        profile["fixture"] = renamed
        self.write(self.head, self.profile, json.dumps(profile))
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertFalse((self.base / self.fixture).exists())
        self.assertEqual((self.base / renamed).read_bytes(), (self.head / renamed).read_bytes())

    def test_old_fixture_still_in_head_is_preserved(self):
        renamed = "internal/runtime/renamed_benchmark_test.go"
        self.write(self.head, renamed, "new benchmark fixture\n")
        profile = json.loads((self.head / self.profile).read_text())
        profile["fixture"] = renamed
        self.write(self.head, self.profile, json.dumps(profile))
        simd_profiles.prepare(self.head, self.base)
        self.assertTrue((self.base / self.fixture).is_file())

    def test_profile_and_fixture_can_be_renamed_together(self):
        renamed = "internal/runtime/renamed_benchmark_test.go"
        (self.head / self.fixture).rename(self.head / renamed)
        profile = json.loads((self.head / self.profile).read_text())
        profile["fixture"] = renamed
        self.write(self.head, "benchmarks/simd/renamed.json", json.dumps(profile))
        (self.head / self.profile).unlink()
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertFalse((self.base / self.fixture).exists())
        self.assertEqual((self.base / renamed).read_bytes(), (self.head / renamed).read_bytes())

    def test_renamed_declared_helper_replaces_old_base_copy(self):
        old = "internal/runtime/old_helper_test.go"
        renamed = "internal/runtime/new_helper_test.go"
        for root, relative in [(self.head, renamed), (self.base, old)]:
            profile = json.loads((root / self.profile).read_text())
            profile["inputs"] = [relative]
            self.write(root, self.profile, json.dumps(profile))
            self.write(root, relative, "shared helper declaration\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertFalse((self.base / old).exists())
        self.assertEqual((self.base / renamed).read_bytes(), (self.head / renamed).read_bytes())

    def test_fixture_has_unique_owner_in_each_revision(self):
        for root in [self.head, self.base]:
            with self.subTest(root=root):
                duplicate = "benchmarks/simd/duplicate.json"
                self.write(root, duplicate, (root / self.profile).read_text())
                with self.assertRaisesRegex(ValueError, "belongs to multiple profiles"):
                    simd_profiles.prepare(self.head, self.base)
                (root / duplicate).unlink()

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

    def workload(self, head_content, base_content):
        relative = "tests/complex/workload.vibe"
        for root, content in [(self.head, head_content), (self.base, base_content)]:
            profile = json.loads((root / self.profile).read_text())
            profile["inputs"] = [relative]
            self.write(root, self.profile, json.dumps(profile))
            self.write(root, relative, content)
        return relative

    def test_changed_workload_is_selected_copied_and_hashed(self):
        relative = self.workload("new workload\n", "old workload\n")
        self.write(self.head, "tests/complex/unselected.vibe", "do not copy\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertEqual((self.base / relative).read_bytes(), (self.head / relative).read_bytes())
        self.assertEqual(plan["groups"][1]["inputs_sha256"][relative], hashlib.sha256(b"new workload\n").hexdigest())
        self.assertFalse((self.base / "tests/complex/unselected.vibe").exists())

    def test_unchanged_workload_does_not_activate_profile(self):
        self.workload("same workload\n", "same workload\n")
        self.assertEqual(simd_profiles.prepare(self.head, self.base)["cases"], 8)

    def test_workload_cannot_escape_checkout(self):
        profile = json.loads((self.head / self.profile).read_text())
        profile["inputs"] = ["../outside.vibe"]
        self.write(self.head, self.profile, json.dumps(profile))
        with self.assertRaisesRegex(ValueError, "invalid repository path"):
            simd_profiles.prepare(self.head, self.base)

    def test_input_cannot_replace_production_go(self):
        profile = json.loads((self.head / self.profile).read_text())
        profile["inputs"] = [self.source]
        self.write(self.head, self.profile, json.dumps(profile))
        with self.assertRaisesRegex(ValueError, "cannot replace production Go"):
            simd_profiles.prepare(self.head, self.base)

    def test_controls_share_inputs_without_selected_profiles(self):
        relative = simd_profiles.CONTROLS["inputs"][0]
        self.write(self.head, relative, "changed control benchmarks\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 8)
        self.assertEqual((self.base / relative).read_bytes(), (self.head / relative).read_bytes())
        self.assertEqual(plan["groups"][0]["inputs_sha256"][relative],
                         hashlib.sha256(b"changed control benchmarks\n").hexdigest())

    def test_shared_helper_changes_are_detected_before_copying(self):
        relative = simd_profiles.CONTROLS["inputs"][0]
        for root in [self.head, self.base]:
            profile = json.loads((root / self.profile).read_text())
            profile["inputs"] = [relative]
            self.write(root, self.profile, json.dumps(profile))
        self.write(self.head, relative, "changed shared helper\n")
        plan = simd_profiles.prepare(self.head, self.base)
        self.assertEqual(plan["cases"], 10)
        self.assertEqual((self.base / relative).read_bytes(), (self.head / relative).read_bytes())

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
