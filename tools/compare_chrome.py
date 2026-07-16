#!/usr/bin/env python3
"""Chrome 差分裁判:surl 的语义树 vs headless Chrome 执行后的 DOM。

warns 只能抓「响的」失败;这里抓静默的——hydration 没跑、树少一块。
两边各抽 heading 文本集合与 link href 集合,算 surl 对 Chrome 的召回率,
低于阈值即标记为待挖的兼容性坑。

Chrome 侧用 --dump-dom + --virtual-time-budget:虚拟时间快进到网络与
定时器静默,是 Chrome 里语义上最接近 surl settledness 的加载终点。

局限(v1):不比 landmark/role(需要真 a11y 树),不比文本正文;
Chrome 用全新临时 profile,无 cookie,遇到反爬墙时两边看到的可能都是
挑战页——低分先人工看一眼再定性。

用法:
    python3 tools/compare_chrome.py                 # 默认站点列表(同 sweep)
    python3 tools/compare_chrome.py URL [URL ...]
    SURL_BIN=... CHROME_BIN=... python3 tools/compare_chrome.py
"""

import html.parser
import os
import re
import subprocess
import sys
import urllib.parse

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SURL = os.environ.get("SURL_BIN", os.path.join(REPO, "target", "release", "surl"))
CHROME = os.environ.get(
    "CHROME_BIN", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
)
TIMEOUT = 90
RECALL_ALERT = 0.7

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
    "https://readaware.app",
    "https://getcontextflow.app",
]

HEADING_RE = re.compile(r'^\s*heading\[\d\] "(.*)"$')
# 名字可空:无可访问名的链接(纯图标/投票箭头)输出为 `link -> url`
LINK_RE = re.compile(r'^\s*link(?: ".*")? -> (\S+)$')


def norm_text(s):
    return " ".join(s.split())


def norm_href(href, base):
    try:
        u = urllib.parse.urlsplit(urllib.parse.urljoin(base, href.strip()))
        # 空路径补 "/":surl 侧的 url crate 总会补,原始 attr 常常没有
        return urllib.parse.urlunsplit(u._replace(path=u.path or "/"))
    except ValueError:
        return href.strip()


def surl_facts(url):
    p = subprocess.run([SURL, url], capture_output=True, text=True, timeout=TIMEOUT)
    headings, links = set(), set()
    for line in p.stdout.splitlines():
        if m := HEADING_RE.match(line):
            if t := norm_text(m.group(1)):
                headings.add(t)
        elif m := LINK_RE.match(line):
            links.add(norm_href(m.group(1), url))
    return headings, links


class DomFacts(html.parser.HTMLParser):
    """从 Chrome dump 的 HTML 里抽 h1-h6 文本与 a[href]。"""

    def __init__(self, base):
        super().__init__(convert_charrefs=True)
        self.base = base
        self.headings, self.links = set(), set()
        self._h_depth = 0
        self._h_text = []
        self._skip_depth = 0  # script/style/template/noscript 内部不算

    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style", "template", "noscript"):
            self._skip_depth += 1
            return
        if self._skip_depth:
            return
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
            self._h_depth += 1
        if tag == "a":
            href = dict(attrs).get("href")
            if href and not href.startswith(("javascript:", "mailto:", "tel:")):
                self.links.add(norm_href(href, self.base))

    def handle_endtag(self, tag):
        if tag in ("script", "style", "template", "noscript"):
            self._skip_depth = max(0, self._skip_depth - 1)
            return
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6") and self._h_depth:
            self._h_depth -= 1
            if self._h_depth == 0:
                if t := norm_text("".join(self._h_text)):
                    self.headings.add(t)
                self._h_text = []

    def handle_data(self, data):
        if self._h_depth and not self._skip_depth:
            self._h_text.append(data)


def chrome_facts(url):
    # 不传 --user-data-dir:macOS 上 headless + 全新临时 profile 会挂死
    # (实测 Chrome 150);默认即临时会话,不影响用户正开着的 Chrome。
    p = subprocess.run(
        [
            CHROME,
            "--headless",
            "--dump-dom",
            "--virtual-time-budget=15000",
            "--disable-gpu",
            "--mute-audio",
            url,
        ],
        capture_output=True,
        text=True,
        timeout=TIMEOUT,
    )
    parser = DomFacts(url)
    parser.feed(p.stdout)
    return parser.headings, parser.links


def recall(surl_set, chrome_set):
    if not chrome_set:
        return None  # Chrome 侧为空,无从谈召回
    return len(surl_set & chrome_set) / len(chrome_set)


def fmt(r):
    return "-" if r is None else f"{r:.0%}"


def main():
    sites = sys.argv[1:] or DEFAULT_SITES
    rows, findings = [], []
    for url in sites:
        try:
            s_head, s_link = surl_facts(url)
        except subprocess.TimeoutExpired:
            rows.append((url, "surl TIMEOUT", "", "", "", ""))
            continue
        try:
            c_head, c_link = chrome_facts(url)
        except subprocess.TimeoutExpired:
            rows.append((url, "chrome TIMEOUT", "", "", "", ""))
            continue
        rh, rl = recall(s_head, c_head), recall(s_link, c_link)
        flag = ""
        if (rh is not None and rh < RECALL_ALERT) or (rl is not None and rl < RECALL_ALERT):
            flag = "⚠"
            findings.append((url, rh, rl, c_head - s_head, c_link - s_link))
        rows.append((url, flag, f"{len(s_head)}/{len(c_head)}", fmt(rh),
                     f"{len(s_link)}/{len(c_link)}", fmt(rl)))
        print(f"done {url} headings={fmt(rh)} links={fmt(rl)} {flag}",
              file=sys.stderr, flush=True)

    print("\n| site | ⚠ | headings surl/chrome | recall | links surl/chrome | recall |")
    print("|---|---|---|---|---|---|")
    for r in rows:
        print("| " + " | ".join(str(x) for x in r) + " |")

    for url, rh, rl, miss_h, miss_l in findings:
        print(f"\n--- {url} (headings {fmt(rh)}, links {fmt(rl)})")
        for t in sorted(miss_h)[:8]:
            print(f"  missing heading: {t[:100]}")
        for t in sorted(miss_l)[:8]:
            print(f"  missing link: {t[:140]}")


if __name__ == "__main__":
    main()
