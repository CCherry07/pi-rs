# pi CLI

`pi` is the terminal coding-agent product built by `pi-rs`. It can inspect a repository, edit files,
run commands, search code, manage long-running sessions, and work with multiple model providers from
one interactive terminal interface.

The CLI is a Rust implementation of current Pi behavior rather than only an agent-loop library. Its
fullscreen TUI, one-shot output, Pi-compatible NDJSON and stdin/stdout RPC, tools, sessions, model
catalog, skills, and plugins all use the same production runtime.

## Product highlights

- **Interactive coding workspace** — fullscreen Ratatui UI with streamed Markdown, syntax-highlighted
  code, CJK/IME input, command completion, history, scrolling, whole-screen mouse selection, and
  clipboard copy.
- **Repository-aware agent** — built-in `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`,
  `find`, `ls`, `memory`, and `session_search` tools for understanding projects and carrying durable
  user-approved context between sessions.
- **Multiple providers and models** — built-in OpenAI-compatible, OpenAI Codex, Anthropic, Google
  Gemini and Vertex, xAI, Mistral, Azure OpenAI, Amazon Bedrock, OpenRouter, and GitHub Copilot
  integrations, plus custom providers and models declared in `models.json`.
- **Authentication in the product** — `/login` and `/logout` manage Pi-compatible credentials from
  the TUI; browser/device OAuth and hidden API-key prompts are supported.
- **Persistent Pi v4 sessions** — resume previous work, import Pi coding-agent v1/v2/v3 files,
  queue steering or follow-up messages, branch from earlier messages, navigate the session tree,
  and compact long contexts.
- **Plugin-first customization** — Rust native plugins, Pi-compatible JavaScript/TypeScript
  extensions, skills, commands, provider hooks, and session lifecycle hooks.
- **Project safety** — nearest-ancestor project-trust decisions gate project `.pi` resources,
  extensions, skills, and native plugins before they load.
- **Automation-friendly frontends** — use the same agent through the interactive TUI, final-text
  `--print` output, Pi-compatible NDJSON `--json` events, bidirectional `--mode rpc`, or ACP stable
  v1 with `--acp`.

## Install and start

### npm package

The npm package is the recommended installation when JavaScript/TypeScript extension support is
needed. It requires Node.js 20 or newer and installs both `pi` and `pi-rs` commands.

```bash
npm install --global @pi-rs/cli
pi --version
pi
```

The package selects a native optional dependency for the current OS, CPU, and Linux libc. Supported
release targets are macOS arm64/x64, Linux glibc arm64/x64, and Windows MSVC arm64/x64. If npm
skips that optional dependency, the launcher prints exact npx and global-install repair commands
for the installed CLI version and platform, including the public registry override needed when a
mirror is stale.

### Run from source

The repository pins Rust 1.98.0 through `rust-toolchain.toml`.

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs
npm install --prefix packages/pi
./scripts/pi-dev
```

`scripts/pi-dev` incrementally builds the current host NAPI library and explicitly selects it for
the Node launcher. The standalone Rust adapter does not embed a JavaScript VM; when JavaScript
extension configuration is active it exits with an actionable launcher error instead of silently
omitting extensions. Use `cargo run -p pi-cli -- --no-extensions` for an intentional native-only
run.

## First use

Start the TUI in a project, configure a provider, and select an available model:

```bash
pi --cwd /path/to/project
```

Then run these commands inside the TUI:

```text
/login
/model
```

`/login` opens a provider selector. API keys are read without echo; OAuth providers open their
browser or device flow. Authentication temporarily switches out of the fullscreen UI and returns to
the current session when complete. The session generation is rebuilt automatically so `/model`
immediately reflects the new credentials.

Authentication can also be managed before entering the TUI:

```bash
# Select a provider and authentication method interactively
pi auth login

# Start a browser/device OAuth flow directly
pi auth login anthropic --oauth
pi auth login openai-codex --oauth
pi auth login github-copilot --oauth
pi auth login openrouter --oauth
pi auth login xai --oauth

# Configure a cloud credential chain
pi auth login amazon-bedrock
pi auth login google-vertex

# Prompt for an API key without echo
pi auth login anthropic --api-key
pi auth login google --api-key

# Inspect credential metadata without printing secrets
pi auth status

# Remove only the stored credential for a provider
pi auth logout anthropic
```

Credentials are stored in Pi-compatible `<agent-dir>/auth.json` with file locking, atomic
replacement, and mode `0600` on Unix. `/logout` removes stored credentials only; environment
variables and credentials declared in `models.json` remain unchanged.

Environment variables are supported as an alternative, including `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_CLOUD_API_KEY`, `XAI_API_KEY`, `MISTRAL_API_KEY`,
`AZURE_OPENAI_API_KEY`, `OPENROUTER_API_KEY`, and `COPILOT_GITHUB_TOKEN`. Vertex also supports ADC
and a service-account file; Bedrock supports its AWS profile/static credential chain and
`AWS_BEARER_TOKEN_BEDROCK`. Explicit CLI credentials take precedence over stored credentials.

## Product modes

| Mode            | Command                         | Best for                                                    |
| --------------- | ------------------------------- | ----------------------------------------------------------- |
| Interactive TUI | `pi`                            | Daily coding, exploration, edits, and long-running sessions |
| Main-screen TUI | `pi --no-fullscreen`            | Terminal-native selection and main-screen output            |
| Final text      | `pi --print "prompt"`           | Shell scripts and one-shot answers                          |
| NDJSON events   | `pi --json "prompt"`            | Integrations that consume structured product events         |
| Stdio RPC       | `pi --mode rpc`                  | Long-lived bidirectional Pi protocol integrations           |
| ACP stable v1   | `pi --acp --no-extensions`       | Zed and other Agent Client Protocol clients                  |
| Piped input     | `printf 'prompt' \| pi --print` | Unix pipelines and generated prompts                        |

`--json` writes the Pi coding-agent v3 session header followed by delta-only Pi session events.
`--mode rpc` accepts strict LF-delimited JSON commands and emits correlated responses plus the same
event projection. JavaScript extension UI request/response remains a separate compatibility gap;
unsolicited `extension_ui_response` messages are currently ignored.

`--acp` serves the official ACP stable-v1 JSON-RPC protocol over stdin/stdout. It supports new,
prompt, cancel, load, resume, list, and close, streams text/thought/tool updates, exposes model and
thinking selectors, and accepts per-session stdio MCP servers. ACP sessions use the normal Pi v4
store, while client-provided MCP configuration is transient and must be provided again on
load/resume. ACP mode currently requires `--no-extensions` when JavaScript/TypeScript extensions
would otherwise be active; native Rust plugins continue to work.

Shell shorthand works in interactive and one-shot frontends and does not require provider
credentials; RPC exposes the corresponding `bash` command:

```bash
# Run a command and include its output in agent context
pi --print '!git status --short'

# Run a command without adding its output to agent context
pi --print '!!cargo test -p pi-core'
```

## Interactive workflow

While the agent is idle, `Enter` submits a new prompt. While it is working, `Enter` sends steering
input into the active turn and `Alt+Enter` queues a follow-up for the next turn. Tool calls, provider
errors, queued input, compaction, and session changes remain visible in the same transcript.

The startup card lists successfully resolved JavaScript/TypeScript packages by their effective
`settings.json` `packages` source (for example `npm:@narumitw/pi-lsp@0.49.5`) instead of guessing a
name from an entry path such as `dist/index.ts`. Explicit files and automatic `.pi/extensions`
entries still use compact path labels. Its Rust plugin row contains only successfully loaded native
plugins configured through global or trusted-project `plugins.json`; built-in Rust plugins and
explicit `--plugin` paths are intentionally omitted. Both rows refresh after `/reload` and session
replacement.

Common commands:

| Command                       | Purpose                                                               |
| ----------------------------- | --------------------------------------------------------------------- |
| `/login [provider]`           | Configure OAuth or API-key authentication                             |
| `/logout`                     | Select and remove a stored provider credential                        |
| `/model [provider/model\|id]` | Inspect or change the active model                                    |
| `/thinking <level>`           | Change reasoning depth for the active model                           |
| `/new [path]`                 | Start a new session                                                   |
| `/resume [query\|path]`       | Find and continue a previous session                                  |
| `/export [file]`              | Export the active branch as HTML or `.jsonl`                           |
| `/import <file.jsonl>`        | Import and resume a Pi v1/v2/v3 or pi-rs v4 session                   |
| `/share`                      | Share an HTML snapshot through a secret GitHub Gist                    |
| `/compact [instructions]`     | Compact the current context, optionally with guidance                 |
| `/fork`                       | Branch before a selected previous user message                        |
| `/clone`                      | Clone the session at its current position                             |
| `/tree`                       | Navigate the current session tree                                     |
| `/name [name]`                | Show or set the session name                                          |
| `/session`                    | Show session path, ID, messages, tokens, and cost                     |
| `/memory-local-*`             | Inspect, search, or rebuild the bundled local-memory index             |
| `/reload`                     | Atomically rebuild plugins, models, resources, and session extensions |
| `/trust`                      | Review or change trust for the current project                        |
| `/copy`                       | Copy the last completed assistant response                            |
| `/clear`                      | Clear the visible transcript without deleting the session             |
| `/help`                       | Show the command list                                                 |
| `/quit`                       | Exit the application                                                  |
| `/skill:<name>`               | Invoke a discovered skill                                             |

Plugin-provided commands are added to the same command palette as built-in commands and skills.

Key bindings:

| Key                      | Action                                                                 |
| ------------------------ | ---------------------------------------------------------------------- |
| `Enter`                  | Complete a selected command, submit while idle, or steer while running |
| `Alt+Enter`              | Queue a follow-up message                                              |
| `Ctrl+J`                 | Insert a newline                                                       |
| `Up` / `Down`            | Select a command or browse input history                               |
| `Tab`                    | Complete the selected command or skill                                 |
| `PageUp` / `PageDown`    | Scroll the transcript                                                  |
| `Ctrl+End`               | Jump back to the latest transcript content                             |
| Mouse drag               | Select and copy any visible TUI text in fullscreen mode                |
| `Cmd+C` / `Ctrl+Shift+C` | Re-copy the selection when the terminal forwards the shortcut         |
| `Esc`                    | Close a focused view or interrupt active work                          |
| `Ctrl+C`                 | Close a view, clear the editor, interrupt work, or quit while idle     |
| `Ctrl+D`                 | Quit while idle with an empty editor                                   |

Fullscreen selection is application-owned because mouse capture is needed for transcript scrolling.
It covers the complete final frame, including the transcript, startup card, composer, completion
panel, focused bottom views, project-trust prompt, context panel, and footer. Selection highlighting
is painted after every widget, and active selection pauses animation ticks so copyable text remains
stable. Releasing a non-empty drag writes the selected text immediately because terminal emulators
commonly consume their native copy shortcuts instead of forwarding them to a fullscreen TUI.
`Cmd+C` / `Ctrl+Shift+C` remain best-effort re-copy shortcuts for terminals that do forward them.
`--no-fullscreen` does not capture the mouse and leaves visible-text selection to the terminal.
`/copy` remains a separate semantic operation that copies the last completed assistant response
without requiring a visible selection.

Clipboard writes use the local native clipboard first. Over SSH they use tmux forwarding when
available and then OSC 52 so the text reaches the local terminal; WSL additionally falls back to
PowerShell. OSC 52 payloads are bounded before encoding.

## Models and providers

The default agent directory is `~/.pi/agent`. Override it with `PI_AGENT_DIR` or `--agent-dir`.
Register custom providers and models in `<agent-dir>/models.json`:

```jsonc
{
  // Comments are supported.
  "providers": {
    "openai-compatible": {
      "api": "openai-completions",
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "$OPENAI_API_KEY",
      "models": [
        {
          "id": "gpt-4o-mini",
          "name": "GPT-4o mini",
          "reasoning": false,
          "input": ["text", "image"],
          "contextWindow": 128000,
          "maxTokens": 16384
        }
      ]
    }
  }
}
```

Environment references and shell-command values in model configuration are resolved only when a
request is sent; credentials are not copied into the public model catalog. Custom routes support
`openai-completions`, `openai-responses`, `azure-openai-responses`, `mistral-conversations`,
`anthropic-messages`, `google-generative-ai`, `google-vertex`, and
`bedrock-converse-stream`. These routes retain their protocol-specific endpoint, authentication,
thinking, image, tool, and streaming behavior instead of treating every service as generic OpenAI.

Pi-compatible provider/model/upsert/override precedence is preserved. Model overrides support all
catalog fields, including partial cost updates, per-key `samplingParams`, headers, and typed
protocol `compat`. Pricing tiers use the highest matching total-input threshold for the whole
request. Compatibility settings alter request serialization, streaming expectations, caching,
routing, thinking, deferred tools, and session-affinity behavior rather than remaining metadata.
Dynamic `oauth: "radius"` catalogs still require a future OAuth/remote-catalog/`pi-messages`
provider implementation and are rejected explicitly for now.
Initial model selection uses this priority:

1. explicit `--model` / `--provider` arguments;
2. the model restored from a session, if still available;
3. the catalog default from `models.json`;
4. the runtime fallback.

After editing `models.json`, run `/reload`. A candidate generation is prepared and validated before
it replaces the active generation, so invalid configuration leaves the current session working.

## Sessions and context

Sessions use the Pi v4 JSONL format. A new session exists in memory immediately, but its file is
created only after the first assistant response completes. Quitting before that response, or using
only shell shorthand, does not leave an empty resume entry.

The product supports:

- session discovery and `/resume`;
- portable active-branch JSONL export and transactional Pi v1/v2/v3 or pi-rs v4 JSONL import;
- self-contained HTML export and secret-Gist sharing through the authenticated GitHub CLI;
- durable steering and follow-up queues;
- branching with `/fork`, `/clone`, and `/tree`;
- manual and automatic context compaction;
- model and thinking-level restoration;
- interrupted-state reduction and provider-context repair;
- preservation of matching tool calls and tool results across persistence and replay.

`/export`, `/import`, and `/share` are registered by the built-in Rust
`SessionTransferPlugin`, not dispatched by CLI command-name branches. Storage and HTML
serialization policy lives in that plugin; `pi-session` only exposes the session-log and migration
primitives it composes. The TUI only renders the generic semantic confirmation used by the import
command.

`/export` writes HTML by default; a destination ending in `.jsonl` writes a portable v4 session.
`/import` accepts Pi coding-agent v1/v2/v3 and native pi-rs v4 JSONL. Legacy files are converted into
a new v4 destination without modifying the source; tree identity, parent links, custom messages,
compaction context, and unknown agent-message extensions are retained. The complete runtime
generation is prepared before switching, and a failed replacement removes the staged destination.

`/share` requires an authenticated `gh` CLI (`gh auth login`). It exports a temporary HTML snapshot,
creates a non-public Gist, and prints both the viewer and Gist URLs. Set `PI_SHARE_VIEWER_URL` to
override the default `https://pi.dev/session/` viewer. Review the transcript before sharing because
tool output can contain source code, local paths, or credentials.

## Delegated child sessions

The built-in `subagent` tool delegates one task to a fresh isolated Pi session and returns the
child's final response to the parent. It provides `scout`, `worker`, `reviewer`, `oracle`, and
`delegate` role prompts. Independent calls emitted in the same assistant turn run through the
normal parallel tool scheduler. Authorized children receive the same tool and may delegate
recursively, with feature-owned limits of default depth 1, 64 cumulative children per root session,
and 20 active children.
The current implementation is foreground-only: the parent tool call remains open until the child
finishes or is aborted. The frontend continues to render its original primary session.

Set the global nesting limit in `<agent-dir>/extensions/subagent/config.json`; the default agent
directory therefore uses `~/.pi/agent/extensions/subagent/config.json`:

```json
{ "maxSubagentDepth": 6 }
```

`PI_SUBAGENT_MAX_DEPTH` overrides that value for the current process when it contains a
non-negative integer. Invalid environment values are ignored. A value of `0` disables subagent
launches at the primary session. When neither source is set, the default is `1`. The effective limit
is captured in each child lineage, so a running child keeps its inherited ceiling across reloads.

The generation-local subagent catalog overlays the built-ins with recursively discovered Markdown
agent definitions from `<agent-dir>/agents/` and, when project trust permits it, the nearest
`.pi/agents/` directory. Project definitions have higher precedence and reload atomically with the
runtime generation. Each file has a YAML frontmatter block followed by its role system prompt:

```markdown
---
name: project-scout
description: Inspect this project and return exact code evidence
aliases: project-explorer
tools: read, grep, find, ls, bash
excludeTools: bash
model: inherit
thinking: off
systemPromptMode: append
inheritSkills: false
skills: review-checklist
skillPath: ./private-skills
timeoutMs: 900000
allowNestedSubagents: false
maxSubagentDepth: 2
---

Inspect the assigned paths and return a concise, evidence-backed result.
```

Supported fields are `name`, `description`, `aliases`, `systemPromptMode` (`append` or `replace`),
`allowNestedSubagents`, `maxSubagentDepth`, `tools`, `excludeTools`, `model`, `thinking`,
`inheritSkills`, `skills`, `skillPath`, and `timeoutMs`. Custom definitions default to `replace`,
do not inherit the normal skill catalog, and cannot delegate recursively unless they opt in. An agent-level
`maxSubagentDepth` is an absolute, non-negative child-lineage ceiling and can only tighten the
inherited global or parent limit. Omitted runtime fields inherit the immediate parent. `tools` is a
strict allowlist, while an explicitly empty value selects no tools; it can never exceed the
parent's active-tool ceiling. `excludeTools` removes exact names after inherited or explicit tool
selection; unknown names have no effect. Aliases are accepted anywhere an agent name is selected,
while prompts, events, and results retain the canonical name. Exact canonical names beat aliases,
and ambiguous alias-to-alias collisions fail candidate generation.

`inheritSkills: true` projects the normal generation-local skill catalog into the child. `skills`
adds explicitly named skills regardless of inheritance. `skillPath` supplies invocation-private
skill files or directories, resolved relative to the agent Markdown file; local matches beat the
normal catalog and never enter the parent catalog. Missing selected skills produce non-fatal
warnings in the subagent result. A skill-enabled explicit tool selection receives `read`
automatically only when `read` remains inside the parent's capability ceiling and is not excluded.

`timeoutMs` must be a positive integer. It bounds the foreground child execution wait after launch;
expiration aborts the isolated session and returns a terminal `timed_out` tool result. `model` accepts `inherit`, an exact
`provider/model`, or a bare available id that prefers the current provider. `thinking` accepts
`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, or `false` for off. Invalid, ambiguous,
unavailable, or model-incompatible selections fail before the child session is created.

For copy-paste real-model verification of direct delegation, recursive children, parallel calls,
and primary-session ownership, follow the [subagent smoke test](../../docs/subagents-smoke-test.md).

## Memory

The default provider follows the memory/self-improvement flow of
[Hermes Agent](https://github.com/NousResearch/hermes-agent/tree/e629c900a87622ddcc31f67a4b4a756b239fbaf0).
It freezes `MEMORY.md` and `USER.md` from `<agent-dir>/pi-hermes-memory/` into the session prompt.
The `memory` tool supports add/replace/remove or atomic batches, targeting `memory` (2,200
characters by default) or `user` (1,375). Successful writes affect subsequent session snapshots.
If memory is full, the current Agent receives existing entries and can consolidate them and retry.
Consolidation failures share a counter across memory/user targets within one invocation. The first
three failures allow correction and retry; the fourth returns `done: true` and instructs the Agent
to continue its user-facing answer without retrying memory. It does not terminate the Agent loop.
A successful write resets the counter, and each new foreground or background invocation starts fresh.

Memory review is due every 10 user requests; skill review is due every 10 model iterations, with
skill-tool use resetting that counter. After a successful final answer, due work is combined into
one background Agent. It inherits the effective prompt, structured history, model, and current
provider authentication. It can read/search files, maintain memory, and create/update reusable
skills. Shell/general file writes are denied unless explicitly opted in and active in the parent.
The review is private: no extra resume entry, no review transcript in the dialogue, no recursive
review. New user requests cancel it and take priority. There is no direct/isolated transport switch
or CLI subprocess fallback.

After its first provider response, a long review can summarize older context in place while
retaining the current request and complete recent tool-call/result groups. The summary uses the
same provider/authentication, has no tools, and shares the review's input-token/time budget. It
cannot modify the main conversation, write a compaction record, or rotate a session. Missing model
window metadata disables this optimization, not the review. Ordinary managed subagents skip
automatic memory/skill reviews (including opt-in lifecycle flushes), while keeping memory injection
and normal memory/skill tools. Reload and nested delegation preserve that distinction.

Skills created through the tool are marked agent-owned. Background modifications require
agent ownership, no external edits, no pin, and a read of the exact file in this review. Deletion
requires verified consolidation into another skill and archives the original. Skills are stored
under `<agent-dir>/pi-hermes-memory/skills/<slug>/SKILL.md`, or
`<agent-dir>/projects-memory/<project>/skills/<slug>/SKILL.md` for explicit project scope.
Reload before using a newly created `/skill:<name>` command. Project skill discovery remains
subject to Pi project trust.

Useful retained Pi commands include `/memory-insights`, `/memory-preview-context`,
`/memory-consolidate`, `/memory-skills`, `/memory-index-sessions`, and
`/memory-sync-markdown`. `memory_search` and `session_search` retain searchable existing data.
These commands, project/failure notes, and optional standing instructions are Rust product
extensions, not Hermes Agent's exact CLI. Correction regexes no longer trigger automatic writes.
Extra pre-compaction/shutdown flushes are opt-in, off by default.

Use `"provider": "local"` to select the independent semantic-memory provider described below.
See [architecture](../../docs/architecture.md#memory-systems) for the pinned baseline, verified
mechanisms, and remaining Rust-specific adaptations; this is not a byte-for-byte Hermes port.

## Local semantic memory

The built-in `memory` tool records explicit facts, preferences, decisions, instructions, and
summaries in the current Pi v4 session before updating a local SQLite index. It supports
`remember`, `correct`, `forget`, `list`, and `search`; `session_search` searches user/assistant text
from active branches of sessions in the current project. Recall is injected only into the current
provider request and is never copied into session history. Automatic capture is off, and the tool
must not be used for passwords, API keys, tokens, private keys, or other secrets.

The derived database is `<agent-dir>/memory/memory.sqlite3`. Current-session JSONL entries of type
`pi.memory.v1` are reconciled when a session starts and settles, so an index failure does not replace
the journal as the commit point. The user-facing management command supports:

```text
/memory-local-status                 # health and row counts
/memory-local-list [query]           # active records in the current scopes, with provenance
/memory-local-search <query>         # explicit search
/memory-local-rebuild                # atomically rebuild from configured v4 session directories
/memory-local-model-status           # local embedding assets and active ranking mode
/memory-local-model-install          # download, verify, backfill, and activate dense recall
/memory-local-model-backfill         # repair active records with missing vectors
```

Rebuild reads JSONL without repairing an actively written torn tail, skips legacy v1-v3 files until
they are imported, and leaves the old index intact if any v4 source is invalid. If SQLite detects a
corrupt database while loading the generation, pi-rs preserves it as `memory.sqlite3.corrupt-*`,
creates a clean derived database, and reports the recovery through `/memory-local-status`; run
`/memory-local-rebuild` to repopulate every saved session.

Dense recall never triggers an implicit first-query download. Enable it either with the explicit
model-install command or with the local provider's automatic initialization policy.
The pinned `intfloat/multilingual-e5-small` ONNX model and tokenizer assets occupy about 465 MiB
under `<agent-dir>/models/embeddings`. Installation verifies every SHA-256 digest before publishing
the ready marker. Once installed, startup uses BM25 and cosine dense candidates with reciprocal-rank
fusion; missing or invalid assets keep the existing lightweight lexical Hybrid mode. Normal startup,
recall, and writes never access the model host, and a cached installation works offline.

Run the opt-in real-model smoke test against an installed cache with:

```bash
PI_MEMORY_EMBEDDING_CACHE=<agent-dir>/models/embeddings \
  cargo test -p pi-plugin-memory-local --test dense_model_smoke -- --ignored --nocapture
```

`<agent-dir>/memory.json` selects the provider; `settings.json` does not own durable-memory policy:

```json
{
  "version": 1,
  "enabled": true,
  "provider": "hermes",
  "providers": {},
  "recall": {
    "maxRecords": 8,
    "tokenBudget": 1200,
    "timeoutMs": 50
  }
}
```

Hermes settings live in `<agent-dir>/hermes-memory-config.json`; if absent, the
`providers.hermes` object in `memory.json` is used. Relevant defaults:

```json
{
  "memoryCharLimit": 2200,
  "userCharLimit": 1375,
  "nudgeInterval": 10,
  "skillNudgeInterval": 10,
  "reviewEnabled": true,
  "reviewExtraTools": [],
  "reviewMaxInputTokens": 600000,
  "flushOnCompact": false,
  "flushOnShutdown": false
}
```

Set either nudge interval to 0 to disable that review trigger; `reviewEnabled: false` disables
automatic review entirely. A nonpositive `reviewMaxInputTokens` removes the aggregate input budget,
but review still has a 16-iteration and 120-second bound. `llmModelOverride` may name a registered
`provider/model`; `llmThinkingOverride` overrides review reasoning. Different-model review uses a
bounded history digest because it cannot reuse the parent's model cache.

`reviewTransport`, `childExtensionPaths`, `memoryMode`, `memoryPolicyStyle`,
`nudgeToolCalls`, `reviewRecentMessages`, and correction-pattern settings have been removed;
there is no compatibility execution path. Do not enable shell tools through `reviewExtraTools`
unless autonomous shell access is intended. Existing user files remain untouched.

For a deterministic regression run:

```sh
cargo test -p pi-plugin-memory-hermes -p pi-runtime
```

For local experimentation, use a disposable agent directory and set `nudgeInterval` or
`skillNudgeInterval` to 1. Finish a conversation, then inspect `/memory-insights` or
`/memory-skills`; asking a new question should cancel an in-flight review. Do not test mutation
against valuable production memory.

## Skills, resources, and project trust

Global resources load from the agent directory. Project resources are discovered from the current
project and its ancestors using Pi-compatible precedence.

```text
~/.pi/agent/
├── auth.json
├── memory.json
├── models.json
├── settings.json
├── trust.json
├── SYSTEM.md
├── APPEND_SYSTEM.md
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/

<project>/.pi/
├── settings.json
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/
```

Project `.pi` resources are loaded only after the shared trust service approves the project.
Interactive runs prompt when needed; `--print` and `--json` default to untrusted unless a stored
decision or CLI flag decides otherwise. Use `--approve` / `-a` or `--no-approve` / `-na` for a
run-local override.

Like current Pi, `AGENTS.md` and `CLAUDE.md` context discovery is independent of project trust.
Skills under `~/.agents/skills` are also supported.

## Plugins and extensions

### Pi JavaScript/TypeScript extensions

The npm launcher delegates discovery to the Rust `pi-js-package-manager`, using current Pi
PackageManager precedence:

1. repeated `-e` / `--extension` local, npm, or git sources;
2. trusted project `settings.json` extension entries;
3. trusted project `.pi/extensions` auto-discovery;
4. user `settings.json` extension entries;
5. user `extensions` auto-discovery;
6. extension resources from configured local, npm, or git packages.

Package manifests, package filters, ignore files, canonical-path deduplication, missing-package
installation, and `PI_OFFLINE` behavior follow the same discovery layer. `--no-extensions` keeps
explicit `-e` sources but disables settings, package, and automatic discovery.

The Node host receives only the resolved ordered paths; Jiti import and JavaScript callbacks remain
in Node while discovery and install policy remain in Rust.

Package configuration and managed npm/git installs use the same Rust PackageManager:

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

`install` and `remove` use user scope by default; `--local` selects trusted project scope. Exact npm
versions stay pinned during update, while npm ranges and unversioned packages update in their
managed scope. Git updates reconcile the configured ref or the checkout's upstream branch. Bare
`pi update` retains current Pi's self-update meaning; pi-rs does not yet implement self-update, so
use `--extensions` for all JavaScript packages.

```bash
# Installed npm launcher
pi --cwd /path/to/project
pi --no-extensions -e /path/to/extension.ts

# Development launcher
npm install --prefix packages/pi
./scripts/pi-dev --cwd /path/to/project
```

See [`packages/pi/README.md`](../../packages/pi/README.md) for the supported Pi extension API and
explicit compatibility gaps.

### Native Rust plugins

Native plugins are version-locked dynamic libraries loaded from global manifests, trusted project
manifests, or explicit `--plugin` paths.

```bash
pi --plugin /path/to/plugin.dylib
pi plugin install /path/to/package
pi plugin install https://example.com/pi-plugin-release.json
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
pi plugin list
pi plugin sync
pi plugin remove <plugin-id>
```

Pass `-l` to plugin-management commands to operate on the trusted project's `.pi` configuration
instead of the global agent directory. See
[`crates/pi-plugin-sdk/README.md`](../../crates/pi-plugin-sdk/README.md) for the native author API and
[`crates/pi-plugin-manager/README.md`](../../crates/pi-plugin-manager/README.md) for package and
registry formats.

## Development

CLI startup and frontend selection live in `src/lib.rs`; `src/session_factory.rs` assembles the
production runtime generation. Terminal ownership remains in this crate, while reusable agent,
provider, plugin, resource, and session behavior stays in the workspace libraries.

Run the CLI directly during development:

```bash
# Complete Node-hosted product with JavaScript extensions
./scripts/pi-dev

# Intentional native-only modes
cargo run -p pi-cli -- --no-extensions
cargo run -p pi-cli -- --no-extensions --print "summarize this repository"
cargo run -p pi-cli -- --no-extensions --json "list the Rust crates"

# Quality gates
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

## Packaging and release

Build a standalone archive and NAPI binding for the current target:

```bash
cd packages/pi && npm install && cd ../..
./scripts/package-target.sh aarch64-apple-darwin
```

Target builds produce a standalone archive, a platform NAPI binding, and SHA-256 checksums under
`dist/release/`. On macOS or Linux, install a standalone archive with:

```bash
./scripts/install-package.sh

# Or choose an exact archive and destination
INSTALL_DIR=/usr/local/bin ./scripts/install-package.sh \
  dist/release/pi-<version>-<rust-target>.tar.gz
```

The npm release uses a small `@pi-rs/cli` root package plus exact-version native platform packages.
Platform packages publish first and the root package publishes last. Release Please owns the
version/changelog PR; the protected workflow publishes through npm Trusted Publishing OIDC and
verifies every registry tarball before publishing the draft GitHub release. See
[`packages/pi/README.md`](../../packages/pi/README.md#distribution--发布) for the release workflow.

Release artifacts currently use checksums and native smoke tests but are not
Developer-ID/Authenticode signed or notarized. Never include `.env` files, API keys, or OAuth tokens
in a distribution artifact.
