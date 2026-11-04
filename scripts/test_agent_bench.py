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

    def test_adb_setup_steps_run_on_the_host_and_are_not_counted(self):
        ran = []
        fake = FakeClient({"find": ("hit", False)})
        r = ab.run_scenario(fake, {"name": "x", "setup": [{"adb": ["shell", "pm", "clear", "p"]}], "steps": [],
                                   "check": {"tool": "find", "args": {}}}, adb=ran.append)
        self.assertEqual(ran, [["shell", "pm", "clear", "p"]])
        self.assertEqual(r["calls"], 1)

    def test_summary_tokens_are_the_median_run(self):
        runs = [{"name": "x", "ok": True, "calls": 2, "tokens": t, "ms": 1.0} for t in (300, 100, 200)]
        r = ab.summarize(runs)
        self.assertEqual(r["tokens"], 200)
        self.assertEqual(r["tokens_spread"], 200)

    def test_summary_flags_spread_over_five_percent(self):
        runs = [{"name": "x", "ok": True, "calls": 2, "tokens": t, "ms": 1.0} for t in (100, 100, 110)]
        self.assertTrue(ab.summarize(runs)["unstable"])
        runs = [{"name": "x", "ok": True, "calls": 2, "tokens": t, "ms": 1.0} for t in (100, 100, 104)]
        self.assertFalse(ab.summarize(runs)["unstable"])

    def test_failed_adb_setup_fails_the_scenario_and_keeps_going(self):
        def broken(args):
            raise FileNotFoundError("adb")
        fake = FakeClient({"find": ("hit", False)})
        r = ab.run_scenario(fake, {"name": "x", "setup": [{"adb": ["shell", "true"]}], "steps": [],
                                   "check": {"tool": "find", "args": {}}}, adb=broken)
        self.assertFalse(r["ok"])
        self.assertIn("adb", r["setup_error"])

    def test_a_step_whose_reply_lacks_its_expect_fails(self):
        fake = FakeClient({"tap": ("tapped\ntoast: saved", False), "find": ("hit", False)})
        ok = ab.run_scenario(fake, {"name": "x", "steps": [], "check": {"tool": "tap", "args": {}, "expect": "toast:"}})
        self.assertTrue(ok["ok"])
        bad = ab.run_scenario(fake, {"name": "x", "steps": [], "check": {"tool": "find", "args": {}, "expect": "toast:"}})
        self.assertFalse(bad["ok"])
        self.assertIn("toast:", bad["steps"][0]["error"])


if __name__ == "__main__":
    unittest.main()
