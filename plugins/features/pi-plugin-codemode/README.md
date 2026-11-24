# pi-plugin-codemode

`CodemodePlugin` registers `codemode`, a tool that runs JavaScript to compose other tools.
The Rust host embeds native QuickJS using `rquickjs`; Node is not required. Each call gets a
fresh VM and the current generation's scoped catalog. The generic nested-tool executor owns
argument preparation, validation, hooks, scheduling and cancellation. Nested results stay inside
the script; the model sees only the outer result and the script's explicit output.

Coding registers this tool for explicit `defaultTools` selection. An enabled MCP
server with codemode exposure (the default) can append it at startup, reload or resume. Top-level
`autoEnableCodemode: false` in `mcp.json` disables that automatic addition and leaves existing
selection intact. See [MCP configuration](../pi-plugin-mcp/README.md#codemode-scripts) for inheritance
and child tool permissions. Children follow the same activation rule within their allowed catalog.

Models supporting grammar-constrained tools submit raw JavaScript; other providers use
`{ "code": "JavaScript source" }`. The code is an async function body, so both top-level `await`
and `return` work:

```js
const [file, matches] = await Promise.all([
  tools.read({ path: "Cargo.toml" }),
  tools.grep({ path: "plugins/features/pi-plugin-mcp/src", pattern: "McpExposure", limit: 5 }),
]);
text({ manifestPreview: file.split("\n").slice(0, 12).join("\n"), matches });
```

Ordinary tools resolve to their joined text content. Tools with explicit structured projection
(including MCP tools) resolve to that JSON value. An ordinary failed, blocked or invalid call
rejects; a structured error result remains available as structured data. Use `Promise.allSettled`
when independent calls should preserve successful results despite individual failures.

## Script helpers

- `tools.NAME(args)` and `tools["exact name"](args)` call scoped tools. Names normalize to JavaScript
  identifiers; the first registered tool wins a collision, including collisions with an exact name.
  `ALL_TOOLS` lists each normalized identifier once. Missing members report nearby matches.
- `ALL_TOOLS` contains `{ name, description }`, including TypeScript input/output declarations and guidelines.
- `await searchTools(query, { limit?, namespace? })` uses the shared BM25 ranker.
- `await describeTool(name)` returns its description and TypeScript declaration; `await describeNamespace(name)` returns
  the namespace's description, instructions and tools. Unknown names return `undefined`.
- `text(value)`, `console.log/info/warn/error/debug`, and `return value` emit output.
- `image(dataUrl)`, `image({ image_url: dataUrl })`, and `image({ type: "image", data, mimeType })`
  accept base64 PNG, JPEG, GIF and WebP data. Each emitted image is saved to a temporary file and
  its path precedes the image in the result. Repeated images share that file within a script.
- `exit()` ends successfully, including when the script catches its internal unwind signal.
- `store(key, value)` and `load(key)` handle JSON values; `store(key, undefined)` deletes a key.
  Successful scripts append `codemode-store` entries with `{ set, delete }`. Loading uses the
  current branch, so tree navigation, reload and resume preserve branch-specific values. Failed
  scripts discard writes. Standalone contexts retain values only within the current invocation.

An optional first line sets execution/output budgets:

```js
// @options: {"timeout_ms": 30000, "max_output_tokens": 2000}
text(await tools.read({ path: "Cargo.toml" }));
```

## Model operations

The Coding product enables `models`. Independently composed plugins can opt in with
`CodemodeOptions { models: true, ..Default::default() }`; the plain plugin does not expose it.
Scripts query the same generation-local model catalog as the host:

```js
const classifiers = await models.getAvailableOfType("classifier");
const images = await models.getModelsOfType("image", "openrouter");
const model = await models.getModelOfType("classifier", "typesafe", "jev-latest");
```

Types are `chat`, `classifier`, and `image`. The provider filter is optional; `null` also means
all providers. `getModelsOfType` includes registered entries without credentials;
`getAvailableOfType` checks current credential configuration and provider/account filters on each query,
without refreshing OAuth, executing credential commands, or probing remote endpoints. Credentials added,
removed or changed during the session are reflected without reload; registered catalog entries stay
immutable until reload. Availability is not a guarantee of remote service access. An unknown model returns
`undefined`. Configured API keys and request headers are kept out of catalog entries; registered
provider metadata is preserved. Chat models are queryable, while
chat inference continues through the ordinary agent loop.

```js
const [model] = await models.getAvailableOfType("classifier");
if (!model) throw new Error("Configure a classifier model first");
const result = await models.classify(model, {
  state: { text: "The service is unavailable" },
  questions: {
    urgent: {
      type: "bool", instructions: "Does this require immediate attention?",
      criteria: { true: "Service outage", false: "Routine request" },
    },
  },
});
if (result.stopReason !== "stop") throw new Error(result.errorMessage);
text(result.answers);
```

Classifier questions support `choice` (label-to-meaning criteria), `score` (ordered string levels),
and `bool` (`true`/`false` meanings). `state` must be an object and `questions` must be nonempty.
Optional `images` contains `{type: "image", data, mimeType}` blocks. The host validates these
shapes before starting inference.

```js
const [model] = await models.getAvailableOfType("image");
if (!model) throw new Error("Configure an image model first");
const result = await models.generateImages(model, {
  input: [{ type: "text", text: "A small blue bird on a white background" }],
});
if (result.stopReason !== "stop") throw new Error(result.errorMessage);
for (const block of result.output) {
  if (block.type === "image") image(block);
  else if (block.type === "text") text(block.text);
}
```

Image input is a nonempty array of text and/or image blocks. Text blocks preserve optional string
`textSignature`; malformed known metadata is rejected, and arbitrary extra properties are not forwarded.
Generated images remain inside the
script until emitted with `image(block)`; a reminder is appended if none are shown.

Model arguments contribute only `provider` and `id`: the host resolves the registered model again,
ignoring script-supplied URLs, headers, APIs and other metadata. Invalid arguments reject the promise;
provider failures resolve a result with `stopReason: "error"` or `"aborted"` and `errorMessage`.
At most four inference calls run concurrently per script, in admission order. Catalog queries do not
use inference slots or create call rows. Inference rows show the model reference, status and cost,
never input state, prompts or images. Usage is summed into the outer tool result and retained for
completed calls when the parent is interrupted.

Provider plugins can register typed models and implement classification or image generation through
the existing provider lifecycle. Rust extends `models.json` entries with `type: "classifier"` or
`type: "image"`; omitted type means `chat`. These typed entries extend Pi’s current file format;
the script method and provider wire contracts follow its typed model runtime. Reload replaces these
catalogs as part of the same validated generation transaction. Actual requests resolve credentials and
configured headers; availability queries perform only read-only configuration checks. Coding includes
all 46 classifier and 63 image records from the pinned legacy catalog, including supported costs,
input limits, output modalities, headers and classifier compatibility metadata. Environment or
`auth.json` credentials can enable them. OpenAI Decisions requires an API key, not ChatGPT OAuth.
Copilot account filters constrain available models without deleting registered entries.

| Model type | `api` | Endpoint relative to `baseUrl` |
| --- | --- | --- |
| Classifier | `typesafe-system-one` | `/systemone` |
| Classifier | `cloudflare-workers-ai-system-one` | `/run` |
| Classifier | `openai-decisions` | `/decisions` |
| Classifier | `llama-cpp-classify` | `/tokenize`, `/completion` |
| Image | `openrouter-images` | `/chat/completions` |

For example, a local llama.cpp classifier can be added without an API key:

```json
{
  "providers": {
    "local-classifier": {
      "api": "llama-cpp-classify",
      "baseUrl": "http://127.0.0.1:8080",
      "models": [{ "id": "local", "type": "classifier" }]
    }
  }
}
```

The built-in Cloudflare classifiers resolve their account placeholder from current stored credential
environment metadata or `CLOUDFLARE_ACCOUNT_ID`; `CLOUDFLARE_API_KEY` supplies the key. A custom
account-specific `baseUrl` is also supported. Other model IDs can be configured through the same file.

## Limits and compatibility

Scripts have no default deadline. An explicit positive `timeout_ms` up to 2,147,483,647 milliseconds
includes worker admission and tool calls. The process-wide worker limit defaults to 2;
`PI_CODEMODE_MAX_WORKERS` is read dynamically on each admission. Waiters are admitted in arrival
order, can cancel, and wait at most 60 seconds (or the remaining script deadline). Permits stay
held until the native VM actually exits, even if its caller was already cancelled.

The VM has a 256 MiB heap and 1 MiB stack guard on an explicitly sized 8 MiB native thread.
Tool arguments, structured replies and stored values support 512 nested object/array levels (a
root container counts as one). Bounded stack-growing JSON encoding/decoding protects the bridge;
session JSONL documents use a separate 1024-level limit for persistence envelopes. There are no
additional source-size, call-count,
argument, reply, or catalog quotas. The frozen prelude limits production to 16 Mi UTF-16 code units
of text and base64 data and 100,000 output items. Exceeding that limit fails the script even if its
exception is caught. Store values allow 256 Ki JSON characters each, with 1 Mi characters total
counting keys plus serialized values; Unicode values are not rejected by a UTF-8 byte cap.

The default text budget is 10,000 estimated tokens at four UTF-16 code units per token. Final
formatted text above the budget keeps its head and tail without splitting surrogate pairs, and
the complete formatted output is saved to a temporary file exposed as `details.fullOutputPath`.
Images never consume this text budget. Their detected PNG/JPEG/GIF/WebP MIME type replaces the
supplied label, and repeated images share a saved file. Result headers report completion/failure,
wall time and output; failures retain script stacks and a summary of calls already made.

## Model-facing tool declarations

`ConfiguredCodemodePlugin::new(CodemodeOptions { .. })` configures declaration presentation. In
`CodemodeMode::On` (the default), direct tools retain their declarations and gain a script call
example and return-value summary. In `CodemodeMode::Only`, those declarations appear inside
codemode, grouped by namespace, instead of as separate model tools. Every request renders its
current scoped tools without mutating the generation's registered catalog.

Inline declarations share a budget of 3000 estimated tokens. Selection rotates through namespaces
and picks their smallest remaining declarations first. Deferred tools remain discoverable through
the script helpers without entering that description. Schema rendering preserves property
descriptions, constraints, local references, unions and output types; recursive references become
`unknown`, and oversized input schemas fall back after omitting constraint tags. The same renderer
supplies the script discovery helpers, including each tool's prompt guidelines.

`docs_path` points to the readable [script and model API reference](codemode.md). Product
hosts can set it to their installed documentation path.

Even unawaited tool calls start before script completion is observed. Pending calls are cancelled and given
up to one second to finish their result hooks; completed usage is retained. The parent scheduler
also owns these pipelines, so dropping or cancelling the outer tool cannot detach them. Its
bounded cleanup aborts and joins unfinished async pipelines before returning the parent result.
Native tool code must cooperate with cancellation; the runtime cannot forcibly stop synchronous
code that never yields.

The VM exposes no Node, direct filesystem/network access, timers, or module loader. Its native
QuickJS engine runs on a worker thread, not in an OS process or WebAssembly sandbox. Instruction
interrupts and checks between promise jobs stop loops, while host futures and their cancellation
signals stop pending tools. Native operations that do not poll interrupts can delay thread cleanup;
the worker permit remains held until cleanup completes. The heap quota covers the VM, with
the prelude limits retained output; it is not a process-wide RSS limit.

Conformance targets coding-agent codemode and `packages/codemode` at legacy Pi revision
`0ab23c37b074252d04e31f5cfb99491f0613fb80`. Session-store persistence, disk reopen/resume and
selected-branch replay are covered through the native session runtime; the standalone JavaScript
package's separate durable sandbox API is not implemented. The script prelude,
source options, output/results and discovery contracts follow that oracle. The implementation
deliberately uses native QuickJS, not QuickJS-WASM: engine versions, available language intrinsics,
engine-generated diagnostics/stack formatting, native-operation interruption and host timezone
are not an engine-equivalence guarantee. `tests/legacy_conformance.rs` compares executable-oracle
fixtures, normalizing only elapsed time, owned output paths and explicitly marked engine stack frames.
The native JSON bridge requires Unicode scalar strings and checks an explicit structural depth
before enabling stack-growing decoding. Lone UTF-16 surrogates and over-limit arguments/store
writes fail explicitly as terminal sandbox errors; they cannot leave an execution waiting forever.
Already-serialized object/array return output avoids a redundant host decode. Exact V8 diagnostic
wording and lossless lone-surrogate transport are accepted non-goals. Malformed
`@options` JSON diagnostics currently use the Rust JSON parser rather than V8's diagnostic suffix.
Specialized requests reuse the configured host transport retries, including for Decisions HTTP 504
responses. Typed model contexts retain supported metadata, not arbitrary extra JavaScript properties.
The catalog generator checks all specialized records against the pinned source; credential rotation,
account filtering and protocol tests use fake credentials and capturing transports. The codemode
facade fixtures use identical deterministic model capabilities, not real-provider authentication. Codemode and other model-only tools
cannot be called recursively from scripts. Generic runtime generations, session persistence and
nested-tool cancellation remain owned by the Rust host rather than a second JavaScript runtime.

## Executable conformance checks

From the repository root:

```sh
cargo test -p pi-plugin-codemode --test legacy_conformance
python3 scripts/sync-specialized-model-catalog.py --check
node --import ./legacy/pi/packages/coding-agent/src/experimental/source-resolver.ts scripts/sync-codemode-conformance.mjs --check
```

Omit `--check` to regenerate from a clean local legacy checkout. The generator executes the actual
coding-agent tool with fake tools/models, verifies temporary image/spill files, and records the
oracle revision. Cargo consumes the checked-in fixture without Node or the legacy checkout.
Documentation paths are configured identically in both hosts rather than rewriting error messages.
The tests preserve error names/messages, call statuses and side-effect summaries; only elapsed
time, generated paths and explicitly selected engine stack frames are normalized.

## Measuring overhead

Run `cargo run -p pi-coding --example codemode_overhead` (or add `--release` for an optimized build).
The example exercises the real scheduler with deterministic in-process provider responses, reports
first-sample and warm p50/p95 latency, request counts and model-visible tool text bytes. Each script
creates a fresh VM; warm samples reuse only the process. It excludes model inference, network and
session disk I/O.

A local macOS arm64 debug run on 2026-11-16, with 20 warm samples, measured these p50 times:

| Scenario | Direct parallel | Codemode parallel | Separate provider turns |
| --- | ---: | ---: | ---: |
| One immediate tool | 0.34 ms | 1.77 ms | 0.28 ms |
| Eight tools with 10 ms delay each | 13.79 ms | 18.78 ms | 107.02 ms |

The latter case returns 4 KiB per tool. Selecting a short field in the script reduces model-visible
tool text from 32,824 to 89 bytes; both parallel variants use two provider requests, while separate
turns use nine. The savings depend on the workflow and its chosen projection. JS adds engine and
bridge overhead; these debug measurements are not production latency promises.
