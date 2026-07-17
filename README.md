# surl

curl gives you bytes. Browsers give you pixels. **surl gives you structure.**

Point it at a SPA and get the page's real, post-JavaScript semantic structure —
no Chrome, no Playwright, no WebDriver, no browser anywhere. surl is a
hand-rolled browser-lite in Rust: html5ever parses the HTML, QuickJS runs the
scripts, and an owned event loop *knows* — rather than guesses — when the page
has settled. The whole thing is one ~8 MB static binary.

```console
$ surl https://readaware.app
document "ReadAware — Reading that remembers"
  banner
    link "ReadAware" -> https://readaware.app/#top
    navigation
      link "Download" -> https://readaware.app/#download
      link "GitHub" -> https://github.com/ahpxex/read-aware
      link "Discord" -> https://discord.gg/whDrKXwHWU
  main
    heading[1] "Reading that remembers"
    paragraph
      text "ReadAware reads alongside you. It builds memory across your books, ..."
    button "Download" (collapsed)
    ...
```

That page is a React SPA. `curl` sees an empty `<div id="root">`; surl runs the
bundle, hydrates the app, waits for quiescence, and prints what a screen reader
(or an AI agent) would actually find.

## Why

The project exists because of a real failure: verifying a SPA deployment with
bare `curl` produced a false positive — the HTML was fine, the app was broken.
Checking *structure after JS* is the only honest check, and doing it with a full
browser is heavyweight, flaky, and nondeterministic.

**vs. curl** — surl executes JavaScript. You get the page users see, not the
bootstrap shell.

**vs. headless Chrome** —

- **Settledness is a fact, not a heuristic.** surl owns the event loop: when the
  macrotask queue is empty, microtasks are drained, no network is in flight, and
  the DOM has gone quiet, the page is done. No `waitForTimeout(3000)`, no
  `networkidle` guesswork.
- **Deterministic, replayable runs.** Time is virtual and starts at 0.
  `setTimeout(5000)` fast-forwards instantly; two runs of the same input produce
  byte-identical output.
- **One small binary.** No browser install, no driver protocol, no sandbox
  processes. QuickJS adds ~2 MB, not 200.

surl is also a deliberate learning project — async Rust, FFI across a GC
boundary, browser internals, spec reading. It aims to be genuinely useful for
agent workflows and CI checks, not to replace a browser.

## Install

```bash
git clone https://github.com/ahpxex/surl && cd surl
cargo build --release
# binary at target/release/surl
```

## Usage

Input is a URL, a local HTML file, or `-` for stdin.

### Output modes

| Flag | Output |
|---|---|
| `--tree` | Semantic outline — landmarks, headings, links, roles (default) |
| `--dom` | Serialized HTML after JS execution |
| `--json` | Full semantic IR with stable node uids |
| `--md` | Readability-style article extraction as Markdown |

```bash
surl https://example.app                 # semantic tree
surl https://example.app --dom           # post-JS HTML
surl https://example.app --json > a.json # machine-readable snapshot
surl https://blog.example/post --md      # just the article
surl page.html --no-js                   # skip JS, raw server structure
```

The IR follows the accessibility-snapshot shape (role / name / state / href +
stable uid), so agents can switch between surl and real-browser tooling without
relearning the format.

### Observability

`--stats` prints a one-line phase breakdown to stderr:

```
surl: stats: doc 0.8s 1KB | scripts 0 in 0.0s | modules 2p/1e in 2.7s+0.0s | settle 0.8s: 5 timers 1 fetches virtual 0ms | 0 errors | total 4.3s
```

### Structural diff

`surl diff` aligns two semantic trees by stable node uid and reports
added / removed / changed nodes. Inputs can be URLs, local HTML, or `--json`
snapshots you saved earlier. Exit code 0 means no difference, 1 means
difference — friendly to CI and watch loops.

```bash
surl https://example.app --json > before.json
# ... deploy ...
surl diff before.json https://example.app
```

Stable uids are the load-bearing part: node identity survives re-renders, so a
re-hydrated page diffs clean instead of lighting up the whole tree.

### Logged-in pages

surl can render as *you*, by importing session state from your local browser:

```bash
surl https://github.com/notifications --cookies-from-browser chrome
```

Cookies are imported from Chrome / Firefox / Safari / Brave / Edge / Arc
(`any` auto-detects); localStorage additionally for the Chromium family. Only
state relevant to the target site is loaded. On macOS, Chrome's cookie store
triggers a one-time Keychain prompt.

Two escape-hatch subcommands exist for debugging and reuse:

```bash
surl cookies chrome --domain github.com   # Netscape format, curl/yt-dlp compatible
surl storage chrome https://github.com    # localStorage dump for one origin
```

Both print live session tokens in plaintext. Treat the output like a
password.

## How it works

```
URL → fetch (reqwest) → html5ever DOM → QuickJS + Web APIs → event loop to quiescence → semantic IR → tree/dom/json/md
```

- **JS engine**: [quickjs-ng] via [rquickjs] — the C engine is compiled in via
  FFI, not ported. Its explicit job queue (`JS_ExecutePendingJob` is pumped by
  hand) is what makes an owned event loop and a virtual clock possible: the
  engine never runs tasks behind the host's back.
- **Binding layer**: resource-table style. JS holds only integer handles; all
  real DOM state lives in Rust-side arenas. No raw pointers crossing the GC
  boundary means no cross-heap cycles to leak, and a cheap path to swapping
  engines later.
- **DOM**: html5ever for parsing (including `innerHTML` re-entry), Servo's
  `selectors` crate for query matching.
- **Runtime surface**: enough Web API to hydrate real frameworks — ESM module
  loading with concurrent graph prefetch, fetch/XHR, custom elements with real
  upgrades, a light Shadow DOM, TreeWalker, MessageChannel, timers, events, and
  a few thousand lines of carefully ordered bootstrap.

Crates: `surl-dom` (tree + parser + selectors), `surl-runtime` (engine, event
loop, module loader, Web APIs), `surl-core` (fetch, semantic IR, diff, markdown,
cookie/localStorage import), `surl` (CLI).

## Verification — also browser-free

- **WPT slice**: 1,084 vendored [Web Platform Tests] files (dom, scripting,
  webappapis, webmessaging) run through the real load pipeline against a
  filesystem-backed HTTP client. Currently 8,512 / 13,651 subtests pass;
  expectations are a two-way ratchet — a new failure is a regression, a new
  pass must be re-blessed. The suite runs in ~12 s.
- **Golden corpus**: frozen real-world pages rendered offline and compared
  byte-for-byte. The founding test case: readaware.app must hydrate to a tree
  containing its Discord invite link, twice, identically. Plus scaffold fixtures
  for React, Vue, Svelte, Lit, and Next.
- **Chrome differential judge**: a tooling harness that renders the same page in
  Chrome and in surl and flags silent compatibility gaps.

## Scope

Structure only, on purpose:

- No pixel rendering, no CSS cascade. Style-driven visibility (`.hidden`
  classes) is currently ignored; if corpus evidence shows it matters, a minimal
  subset (inline style + `hidden` + `aria-hidden`) may land — never the full
  cascade.
- No anti-bot / fingerprint-evasion arms race.
- No MCP server — a pipe-friendly CLI *is* the agent interface.

## Status

The core pipeline works end to end: real SPAs — React, Vue, Svelte, Lit,
Next — hydrate in the runtime, and everything documented above is implemented,
including structural diff and browser session import. Planned next:
content-addressed snapshot storage (`surl <url> @yesterday`), a watch mode,
and exposing the virtual clock as a CLI flag.

## License

MIT

[quickjs-ng]: https://github.com/quickjs-ng/quickjs
[rquickjs]: https://github.com/DelSkayn/rquickjs
[Web Platform Tests]: https://web-platform-tests.org
