# Security Policy

CALM (binary/crates/packages are all named `calm` — the earlier `ci` naming has been fully retired) runs locally with filesystem write access and can execute external scripts via IDE hooks. We take reports about its security seriously and appreciate responsible disclosure.

## Supported versions

Only the latest tagged release receives security fixes while the project is pre-1.0. Once we reach 1.0, this table will track a real support window.

| Version         | Supported |
|------------------|-----------|
| latest tag       | ✅ |
| anything older   | ❌ |

## Reporting a vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Use GitHub's private advisory flow: **Security** tab → **Report a vulnerability**. If that's unavailable to you, email **gokuderafight@gmail.com**.

Please include:
- A description of the issue and its potential impact
- Steps to reproduce (a minimal repo/config is ideal)
- The commit/tag/version you tested against

We aim to:
- Acknowledge your report within **3 business days**
- Give an initial assessment (confirmed / not applicable / needs more info) within **7 business days**
- Ship a fix or mitigation, or agree on a disclosure timeline with you, within **30 days** for high-severity issues

## Scope — where to look

CALM's design goal is "local-only, no outbound calls for the code/data path," so anything that breaks that guarantee is high priority. Surfaces we specifically want scrutinized:

1. **`scripts/mcp-launcher.sh`** — downloads and runs a prebuilt release binary when checkout is on a matching git tag. It checksum-verifies against `SHA256SUMS`, but "download and exec" is inherently sensitive — report any way this check can be bypassed or spoofed.
2. **Hook scripts** (`.claude/hooks/calm-nudge.sh` today, plus the native `calm init --hooks[=nudge|enforce]` scaffold that generates the PreToolUse/PostToolUse wiring and its `calm-core::hooks`/`hooks_check` backend; VS Code/Cursor/Windsurf adapters are still on the roadmap, see `CONTRIBUTING.md`) — these run with the same OS permissions as the host IDE/agent. A hook that can be tricked into approving a call it should deny, or hijacked into running arbitrary commands, is a critical-severity report.
3. **`edit_lines`/`edit_symbol`** — the one write path. Anything that lets a write bypass the `expected_hash` conflict guard, the syntax validation, or the `confirm:true` requirement on hub-touching/ungrounded edits counts.
4. **The semantic-search model's network fallback** — the default embedding model ships vendored into the `calm` binary (fetched from Hugging Face Hub and checksum-verified at *build* time by `build.rs`), so a normal release binary loads it with zero network I/O at runtime. A live Hugging Face Hub download is only attempted if that vendored asset turns out to be unusable (e.g. an unresolved pointer stub from an old checkout), and only when `semantic_search.allow_network_fallback` permits it. Report anything that lets this runtime fallback path fetch or execute something other than the pinned model, or that triggers a network call despite `allow_network_fallback: false`.
5. **Output sanitization** — if `source`/`understand` ever leak a credential-shaped secret they claim to redact, or a prompt-injection payload in code gets silently *acted on* instead of just flagged.

## Out of scope

- Vulnerabilities that live purely in an upstream dependency with no reachable path through CALM's own code (please still let us know so we can track it, but report upstream too)
- Denial-of-service via a maliciously huge/malformed repo, unless it also causes memory corruption or arbitrary code execution
- Issues requiring physical access to an already-compromised machine

## Coordinated disclosure

We ask for a reasonable window (typically 90 days, or by mutual agreement) to ship a fix before public disclosure. We're happy to credit you by name or handle in the release notes — just tell us your preference.

## Safe harbor

Good-faith security research conducted under this policy — testing against your own local instance, not accessing other users' data, not degrading the service for anyone else — is authorized. We won't pursue legal action for research that stays within this scope.
