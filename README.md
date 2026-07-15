# surl

curl gives you bytes. Browsers give you pixels. **surl gives you structure.**

A CLI that takes a SPA's URL and returns the page's real, post-JavaScript
structure — no Chrome, no Playwright, no browser anywhere. A hand-rolled
browser-lite in Rust: html5ever for the DOM, QuickJS for scripts, an owned
event loop that *knows* (not guesses) when the page has settled.

Status: scaffold. The `fetch` layer works; everything interesting is ahead.

```bash
cargo run -- https://readaware.app
```

## Planned shape

```
URL → fetch → html5ever DOM → QuickJS + Web APIs → event loop to quiescence → semantic tree
```

- No pixel rendering, no CSS cascade — structure only.
- Deterministic runs: virtual clock, fast-forwardable timers.
- Verification without browsers: WPT slices + golden corpus snapshots.
