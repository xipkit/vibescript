"""Regression tests for recording individual golden cases."""

import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import golden


class RecordingTests(unittest.TestCase):
    def test_selected_recording_preserves_other_observations_and_counters(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = SimpleNamespace(inline=False, golden=root / "cases.jsonl", counters=root / "counters.jsonl.gz")
            golden.write_jsonl(corpus.golden, [{"id": "a", "ok": ["nil"]}, {"id": "b", "ok": ["int", "1"]}])
            golden.write_jsonl(corpus.counters, [["a", 1, 2, 3], ["b", 4, 5, 6]])
            golden.record_corpus(corpus, ["a"], {"a": {"compiled": True}}, {}, {}, None, preserve=True)
            self.assertEqual(golden.read_jsonl(corpus.golden), [{"id": "a", "compiled": True}, {"id": "b", "ok": ["int", "1"]}])
            self.assertEqual(golden.read_jsonl(corpus.counters), [["b", 4, 5, 6]])

    def test_selected_lsp_recording_keeps_unselected_reply_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = SimpleNamespace(inline=False, golden=root / "cases.jsonl", counters=root / "counters.jsonl.gz")
            golden.write_jsonl(corpus.golden, [{"id": "a", "replies": [0]}, {"id": "b", "replies": [1]}])
            table = golden.Replies([{"reply": "old"}, {"reply": "keep"}])
            replacement = table.add("new")
            with patch.object(golden, "GOLDEN", root):
                golden.record_corpus(corpus, ["a"], {"a": {"replies": [replacement]}}, {}, {}, table, preserve=True)
            records = golden.read_jsonl(corpus.golden)
            replies = golden.read_jsonl(root / "lsp.replies.jsonl.gz")
            self.assertEqual([[replies[index]["reply"] for index in row["replies"]] for row in records], [["new"], ["keep"]])


if __name__ == "__main__":
    unittest.main()
