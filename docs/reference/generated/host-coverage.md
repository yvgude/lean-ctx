<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

# Context gateway coverage by host

What the context gateway can see and stop for each host when fully set up.
`lean-ctx doctor` shows the coverage actually in effect on this machine.

| Level | Meaning |
|---|---|
| enforced | Model traffic flows through the lean-ctx proxy; egress admission checks every byte before it leaves. A stopped proxy fails the request (fail-closed). |
| partial | lean-ctx tools are admitted and shell commands rewritten; host-native file tools still reach the model unchecked. Rewrite hooks fail open by design. |
| not_observable | MCP only: `ctx_*` calls are admitted, the host's own tools are invisible. |
| unsupported | No integration. |

`observed` (sees every call but cannot stop it) is reserved; no current integration delivers it, so none is claimed.

| Host | Tool integration | Proxy-routable | Best achievable | Still outside the gateway |
|---|---|---|---|---|
| aider | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| amazonq | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| amp | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| antigravity | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| antigravity-cli | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| augment | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| claude | deny native tools | yes | enforced | needs an Anthropic API key (Pro/Max sign-in cannot be proxied); non-model calls pass through unchanged |
| cline | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| codebuddy | deny native tools | no | partial | host-native file tools reach the model without gateway admission |
| codewhale | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| codex | deny native tools | yes | enforced | non-model calls of the host (sign-in, pairing) pass through unchanged |
| continue | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| copilot | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| crush | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| cursor | deny native tools | no | partial | host-native file tools reach the model without gateway admission |
| emacs | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| gemini | deny native tools | no | partial | host-native file tools reach the model without gateway admission |
| grok | MCP only | yes | enforced | non-model calls of the host (sign-in, pairing) pass through unchanged |
| hermes | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| jetbrains | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| kiro | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| neovim | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| omp | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| openclaw | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| opencode | deny native tools | no | partial | host-native file tools reach the model without gateway admission |
| qoder | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| qodercli | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| qoderwork | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| qwen | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| roo | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| sublime | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| trae | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| verdent | shell rewrite | no | partial | host-native file tools reach the model without gateway admission |
| vibe | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| vscode | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| vscode-insiders | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
| windsurf | deny native tools | no | partial | host-native file tools reach the model without gateway admission |
| zed | MCP only | no | not_observable | only ctx_* calls are admitted; the host's own tools are invisible |
