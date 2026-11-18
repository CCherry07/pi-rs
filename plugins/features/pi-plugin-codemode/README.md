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

- `tools.NAME(args)` and `tools["exact name"](args)` call scoped tools. Safe normalized aliases are
  provided only when unambiguous. Exact names always remain accessible.
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

Scripts query the same generation-local model catalog as the host:

```js
const classifiers = await models.getAvailableOfType("classifier");
const images = await models.getModelsOfType("image", "openrouter");
const model = await models.getModelOfType("classifier", "typesafe", "jev-latest");
```

Types are `chat`, `classifier`, and `image`. The provider filter is optional; `null` also means
all providers. `getModelsOfType` includes registered entries without credentials;
`getAvailableOfType` filters by configured provider capabilities. An unknown model returns
`undefined`. Catalog entries contain no API keys or headers. Chat models are queryable, while
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

Image input is a nonempty array of text and/or image blocks. Generated images remain inside the
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
catalogs as part of the same validated generation transaction. Credentials and configured headers resolve only when a request is
made. Coding includes a selected built-in catalog for TypeSafe, OpenAI Decisions and OpenRouter;
existing API keys in the environment or `auth.json` can enable them. OpenAI Decisions requires an
API key; ChatGPT OAuth credentials cannot enable that endpoint.

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

Cloudflare classifiers need a configured account-specific `baseUrl` ending in `/ai` and credentials;
they are not added to the default catalog. Other model IDs can be configured through the same file.

## Limits and compatibility

The defaults are a 60-second deadline (after admission), 256 MiB QuickJS heap, 256 KiB VM stack,
and approximately 10,000 output tokens (four UTF-8 bytes per estimated token). At most two VM
workers run process-wide; admission waits at most 60 seconds and is cancellable. Scripts may set a
positive timeout up to 2,147,483,647 milliseconds and a nonnegative output budget.

Source is limited to 1 MiB. A script may issue at most 128 host calls, including model queries and inference, with at most
1 MiB of JSON arguments per tool call or 8 MiB per model call, and 16 MiB total. Each serialized reply is at most 8 MiB; queued replies share a
32 MiB host-memory budget. Excess replies reject with a message asking for smaller batches. Output
is limited to 16 MiB and 10,000 items. Text is safely truncated at the requested budget and images
that do not fit are omitted. Truncation is reported; this version does not save full output files.
Store values are limited to 256 Ki JavaScript characters, with a 1 MiB authoritative JSON store.

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

At script completion, queued calls stop before execution. Started calls are cancelled and given
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
separate bounded host bridge/output buffers; it is not a process-wide RSS limit.

Compared with `legacy/pi`, this implementation deliberately uses native QuickJS, has a finite
default deadline and explicit bridge/call quotas.
Model availability keeps the Rust host's credential-configuration snapshot rather than resolving
secrets during catalog queries; actual credential failures appear when calling the model. Specialized
requests reuse the configured host transport retries, including for Decisions HTTP 504 responses.
Classifier questions and choice labels use deterministic Rust map ordering rather than JavaScript
insertion order; llama.cpp labels and answers use that same order consistently. The bundled model
catalog is a selected subset of Pi's catalog. Configurable worker limits, streaming yields and
full-output spill files are not yet implemented. Codemode and other model-only tools cannot be called recursively from scripts.

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
