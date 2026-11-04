#!/usr/bin/env python3
"""Unit tests for scripts/agent-bench.py (no device needed)."""
import importlib.util
import os
import unittest

_spec = importlib.util.spec_from_file_location("ab", os.path.join(os.path.dirname(__file__), "agent-bench.py"))
ab = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ab)


class FakeClient:
    """Answers each tool with a fixed (text, is_error)."""

    def __init__(self, replies):
        self.replies = replies

    def call(self, tool, args):
        text, is_error = self.replies[tool]
        return text, is_error, 1.0


class AgentBenchTest(unittest.TestCase):
    def test_approx_tokens(self):
        self.assertEqual(ab.approx_tokens(""), 0)
        self.assertEqual(ab.approx_tokens("abcd"), 1)
        self.assertEqual(ab.approx_tokens("abcde"), 2)
        self.assertEqual(ab.approx_tokens("설정"), 2)
        self.assertEqual(ab.approx_tokens("ab설정"), 3)

    def test_failed_check_or_error_fails_the_scenario(self):
        fake = FakeClient({"open": ("ok", False), "find": ("NOT_FOUND", True)})
        r = ab.run_scenario(fake, {"name": "x", "setup": [], "steps": [{"tool": "open", "args": {}}],
                                   "check": {"tool": "find", "args": {}}})
        self.assertFalse(r["ok"])
        self.assertEqual(r["calls"], 2)

    def test_setup_is_not_counted(self):
        fake = FakeClient({"go": ("ok", False), "find": ("hit", False)})
        r = ab.run_scenario(fake, {"name": "x", "setup": [{"tool": "go", "args": {}}], "steps": [],
                                   "check": {"tool": "find", "args": {}}})
        self.assertTrue(r["ok"])
        self.assertEqual(r["calls"], 1)
        self.assertEqual(r["tokens"], ab.approx_tokens("hit"))


if __name__ == "__main__":
    unittest.main()
