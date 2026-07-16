#!/usr/bin/env python3
"""real-site sweep:对一批真实站点跑 surl,收集墙钟/settle 统计/错误信号。

发现工具,不是回归工具:不进 CI,大改动后手动跑一轮,和上次的表 diff。
发现的异常沉淀成离线回归(corpus / 最小化测试页),这里只负责暴露问题。

用法:
    python3 tools/sweep.py                 # 默认站点列表
    python3 tools/sweep.py URL [URL ...]   # 指定站点
    SURL_BIN=path/to/surl python3 tools/sweep.py
"""

import os
import re
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("SURL_BIN", os.path.join(REPO, "target", "release", "surl"))
TIMEOUT = 90

DEFAULT_SITES = [
    "https://linear.app",
    "https://react.dev",
    "https://vuejs.org",
    "https://svelte.dev",
    "https://astro.build",
    "https://tanstack.com",
    "https://news.ycombinator.com",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://github.com",
    "https://stripe.com",
    "https://vercel.com",
    "https://www.reddit.com",
    "https://readaware.app",
    "https://getcontextflow.app",
]

ANSI = re.compile(r"\x1b\[[0-9;]*m")
SETTLE = re.compile(
    r"page load settled scripts=(\d+) script_errors=(\d+) "
    r"modules_prefetched=(\d+) modules_evaluated=(\d+) timers=(\d+) "
    r"fetches=(\d+) virtual_ms=(\d+)"
)


def run_site(url, env):
    t0 = time.monotonic()
    status = "ok"
    stdout, stderr = "", ""
    try:
        p = subprocess.run([BIN, url], capture_output=True, text=True,
                           timeout=TIMEOUT, env=env)
        stdout, stderr = p.stdout, ANSI.sub("", p.stderr)
        if p.returncode != 0:
            status = f"exit={p.returncode}"
    except subprocess.TimeoutExpired as e:
        raw = e.stderr or b""
        stderr = ANSI.sub("", raw.decode() if isinstance(raw, bytes) else raw)
        status = "TIMEOUT"
    return time.monotonic() - t0, status, stdout, stderr


def main():
    sites = sys.argv[1:] or DEFAULT_SITES
    env = {**os.environ, "RUST_LOG": "surl=debug,surl_js=warn"}
    rows, details = [], []

    for url in sites:
        wall, status, stdout, stderr = run_site(url, env)
        m = SETTLE.search(stderr)
        stats = m.groups() if m else ("-",) * 7
        warns = [l.split("surl_js: ", 1)[-1].strip()
                 for l in stderr.splitlines() if "WARN" in l or "ERROR" in l]
        lines = len(stdout.splitlines())
        rows.append((url, f"{wall:.1f}s", status, lines, *stats, len(warns)))
        if warns or status != "ok":
            uniq = []
            for w in warns:
                if w[:80] not in [u[:80] for u in uniq]:
                    uniq.append(w)
            details.append((url, status, uniq[:8], len(warns)))
        print(f"done {url}: {wall:.1f}s {status} warns={len(warns)}",
              file=sys.stderr, flush=True)

    print("\n| site | wall | status | tree_lines | scripts | script_err "
          "| mod_pre | mod_eval | timers | fetches | virtual_ms | warns |")
    print("|---" * 12 + "|")
    for r in rows:
        print("| " + " | ".join(str(x) for x in r) + " |")

    print("\n=== details (sites with warnings/failures) ===")
    for url, status, uniq, total in details:
        print(f"\n--- {url} [{status}] ({total} warns/errors total)")
        for w in uniq:
            print(f"  * {w[:300]}")


if __name__ == "__main__":
    main()
