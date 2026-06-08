Your agent isn't about code — it's prospecting leads, synthesizing research, or triaging support tickets. The defaults are tuned for software work: the full power tool surface, a code-oriented intent taxonomy, identity compression. A persona reshapes the *whole* context surface for your domain in one switch — and leaves the coding default exactly as it was.

---

## 1. Select a persona

```bash
export LEAN_CTX_PERSONA=lead-gen     # or: research · support · data-analysis · coding
```

## 2. The whole surface changes together

Each persona is a real, shipped bundle — tool surface, read-mode, compressor,
chunker, intent taxonomy and sensitivity floor all move as one:

```text
coding                              → Power profile · identity · lines · auto · floor=public
research                           → Standard profile · markdown · paragraph · map · floor=public
support · data-analysis            → Standard profile · prose/identity · floor=internal
lead-gen (alias: sales)            → Custom 6-tool surface · prose · paragraph · floor=confidential
        tools: ctx_read · ctx_search · ctx_url_read · ctx_knowledge · ctx_semantic_search · ctx_session
```

The lead-gen surface is genuinely *narrowed* to those six prospecting tools — the
refactor/code tools are gone — while `coding` keeps the full Power surface.

## 3. Ship your own vertical — no fork

Not one of the built-ins? Drop a declarative persona file and select it:

```toml
# <personas_dir>/compliance.toml  (default <OS-config-dir>/lean-ctx/personas; override via LEAN_CTX_PERSONAS_DIR)
name = "compliance"
tool_profile = "custom"
tools = ["ctx_read", "ctx_search", "ctx_semantic_search", "ctx_knowledge"]
default_read_mode = "map"
compressor = "prose"
chunker = "paragraph"
intent_taxonomy = ["scan", "flag", "cite", "report"]
sensitivity_floor = "confidential"
```

```bash
export LEAN_CTX_PERSONA=compliance
```

## 4. Under the hood — `rust/src/core/persona.rs`

`Persona::resolve` reads `LEAN_CTX_PERSONA` › `config.persona` › default `coding`;
an unknown name falls back to `coding`, never an error. The resolved profile drives
`list_tools`, so the surface genuinely shrinks or grows per persona. An explicit
tool-profile always wins, so existing coding installs are untouched.

## Payoff

One runtime, many verticals. A sales, research, support or data agent gets a
surface built for its domain — and the `coding` default behaves exactly as before.
