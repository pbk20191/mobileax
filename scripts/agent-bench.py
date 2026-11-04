#!/usr/bin/env python3
"""Agent token/turn benchmark over `mobax mcp`.

Replays scenario files (bench/agent/*.json): each lists the tool calls an
agent needs today for a task, then a check. Counts calls, approximate
response tokens and time per scenario, plus the per-session cost of the tool
list. No LLM runs; results are deterministic and free.

  scripts/agent-bench.py [--mobax EXE] [--device SERIAL] [--runs N]
                         [--out FILE] [--compare BASELINE] [SCENARIO...]

Approximate tokens: ASCII chars / 4 (rounded up) + one per non-ASCII char.
A consistent yardstick for comparing runs, not a real tokenizer; changing it
means re-recording the baseline.
"""
import argparse
import glob
import json
import math
import os
import queue
import statistics
import subprocess
import sys
import threading
import time

TIMEOUT_S = 30


def approx_tokens(text):
    ascii_n = sum(1 for c in text if ord(c) < 128)
    return math.ceil(ascii_n / 4) + (len(text) - ascii_n)


class McpClient:
    """`mobax mcp` over stdio, one JSON-RPC line per message."""

    def __init__(self, exe, device=None):
        cmd = [exe, "mcp"] + (["--device", device] if device else [])
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next_id = 0
        self._request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                     "clientInfo": {"name": "agent-bench", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.p.stdout:
            self.lines.put(line)

    def _send(self, msg):
        self.p.stdin.write((json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8"))
        self.p.stdin.flush()

    def _request(self, method, params):
        self.next_id += 1
        rid = self.next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.monotonic() + TIMEOUT_S
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                return None
            try:
                msg = json.loads(self.lines.get(timeout=left))
            except queue.Empty:
                return None
            if msg.get("id") == rid:
                return msg

    def tools_list_text(self):
        msg = self._request("tools/list", {}) or {}
        return json.dumps(msg.get("result", {}), ensure_ascii=False)

    def call(self, tool, args):
        t0 = time.monotonic()
        msg = self._request("tools/call", {"name": tool, "arguments": args})
        ms = (time.monotonic() - t0) * 1000
        if msg is None:
            return "TIMEOUT", True, ms
        if "error" in msg:
            return json.dumps(msg["error"], ensure_ascii=False), True, ms
        res = msg.get("result", {})
        text = "".join(c.get("text", "") for c in res.get("content", []))
        return text, bool(res.get("isError")), ms

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def adb_runner(device=None):
    """Runs `adb ARGS…` on the host (setup only: MCP has no device shell)."""
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    exe = os.path.join(sdk, "platform-tools", "adb") if sdk else "adb"
    pre = [exe] + (["-s", device] if device else [])

    def run(args):
        subprocess.run(pre + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=TIMEOUT_S)
    return run


def run_scenario(client, scenario, adb=None):
    for s in scenario.get("setup", []):
        if "adb" in s:
            adb(s["adb"])
        else:
            client.call(s["tool"], s.get("args", {}))
    steps = []
    for s in scenario.get("steps", []) + [scenario["check"]]:
        text, err, ms = client.call(s["tool"], s.get("args", {}))
        steps.append({"tool": s["tool"], "args": s.get("args", {}), "tokens": approx_tokens(text), "chars": len(text),
                      "ms": round(ms, 1), "error": text[:200] if err else None})
    return {"name": scenario["name"], "ok": all(s["error"] is None for s in steps), "calls": len(steps),
            "tokens": sum(s["tokens"] for s in steps), "chars": sum(s["chars"] for s in steps),
            "ms": round(sum(s["ms"] for s in steps), 1), "steps": steps}


def summarize(runs):
    """Several runs of one scenario → one row (time is the median)."""
    first = runs[0]
    return {"name": first["name"], "ok": all(r["ok"] for r in runs), "calls": first["calls"],
            "tokens": first["tokens"], "tokens_spread": max(r["tokens"] for r in runs) - min(r["tokens"] for r in runs),
            "ms": round(statistics.median(r["ms"] for r in runs), 1), "runs": runs}


def default_mobax():
    here = os.path.dirname(os.path.abspath(__file__))
    for rel in ("../target/release/mobax.exe", "../target/release/mobax"):
        p = os.path.normpath(os.path.join(here, rel))
        if os.path.isfile(p):
            return p
    return "mobax"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--mobax", default=default_mobax())
    ap.add_argument("--device")
    ap.add_argument("--adb", help="adb for setup steps (default: $ANDROID_HOME/platform-tools/adb, else PATH)")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--out")
    ap.add_argument("--compare")
    ap.add_argument("scenarios", nargs="*")
    a = ap.parse_args()
    root = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    files = a.scenarios or sorted(f for f in glob.glob(os.path.join(root, "bench/agent/*.json"))
                                  if not os.path.basename(f).startswith("baseline"))
    scenarios = [json.load(open(f, encoding="utf-8")) for f in files]

    client = McpClient(a.mobax, a.device)
    try:
        tools = approx_tokens(client.tools_list_text())
        adb = (lambda args: subprocess.run([a.adb] + (["-s", a.device] if a.device else []) + args,
                                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                            timeout=TIMEOUT_S)) if a.adb else adb_runner(a.device)
        rows = [summarize([run_scenario(client, s, adb) for _ in range(a.runs)]) for s in scenarios]
    finally:
        client.close()

    base = {}
    if a.compare:
        b = json.load(open(a.compare, encoding="utf-8"))
        base = {r["name"]: r for r in b["scenarios"]}
        base["__tools__"] = {"tokens": b["tools_tokens"]}

    def delta(new, old):
        return "" if old is None else f" ({new - old:+d})"

    print(f"{'scenario':<24} {'ok':<3} {'calls':>5} {'tokens':>14} {'±':>4} {'ms(p50)':>9}")
    for r in rows:
        o = base.get(r["name"], {})
        print(f"{r['name']:<24} {'y' if r['ok'] else 'N':<3} {str(r['calls']) + delta(r['calls'], o.get('calls')):>5} "
              f"{str(r['tokens']) + delta(r['tokens'], o.get('tokens')):>14} {r['tokens_spread']:>4} {r['ms']:>9}")
    total = sum(r["tokens"] for r in rows)
    print(f"{'tools/list (per session)':<24} {'':<3} {'':>5} {str(tools) + delta(tools, base.get('__tools__', {}).get('tokens')):>14}")
    print(f"{'total (scenarios)':<24} {'':<3} {sum(r['calls'] for r in rows):>5} {total:>14}")
    for r in rows:
        if not r["ok"]:
            bad = next(s for run in r["runs"] for s in run["steps"] if s["error"])
            print(f"FAILED {r['name']}: {bad['tool']} {json.dumps(bad['args'], ensure_ascii=False)}: {bad['error']}", file=sys.stderr)

    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            json.dump({"tools_tokens": tools, "scenarios": rows}, f, ensure_ascii=False, indent=1)
    return 0 if all(r["ok"] for r in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
