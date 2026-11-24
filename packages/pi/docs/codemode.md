# Codemode

The `codemode` tool runs JavaScript that calls tools and non-LLM models. It runs an async function body in a QuickJS sandbox, so top-level `await` and `return` work. Calls still running when the script ends are cancelled.

## Scripts and tool results

`tools.NAME(args)` calls a tool and resolves to the value in its TypeScript declaration. Tools with an output schema return a structured value; other tools return text. A structured error result also resolves, so inspect its fields. Failed, blocked, or invalid calls otherwise reject with an `Error`. Use `Promise.allSettled()` for independent calls.

`ALL_TOOLS` lists callable tools as `{ name, description }`. `await searchTools(query, { limit?, namespace? })` discovers tools, `await describeTool(name)` returns their TypeScript declarations and guidelines, and `await describeNamespace(name)` returns namespace instructions.

`text(value)`, `console.log(...)`, and top-level `return` add output. `image(dataUrlOrImageBlock)` shows a base64 image and saves it to a temp file named in the result. `exit()` ends successfully. `store(key, value)` and `load(key)` keep JSON values on the current session branch; `store(key, undefined)` deletes a value. Only successful scripts save writes.

An optional first line sets the output token budget and deadline:

```js
// @options: {"max_output_tokens": 2000, "timeout_ms": 300000}
```

Scripts have no default deadline. `timeout_ms`, when set, includes waiting for a worker and all tool calls; image generation can take minutes. The default output budget is 10,000 estimated tokens (four UTF-16 code units per token). Truncated text keeps its beginning and end, and the result names a temporary file containing the complete output. Images are retained independently of the text budget.

The native Rust host uses a 256 MiB VM heap and a 1 MiB engine stack guard on an 8 MiB worker thread. Tool arguments, structured replies and stored values support 512 nested object/array levels (the root container counts as one); session persistence has separate envelope headroom. Scripts may produce at most 16 Mi UTF-16 code units of text/image data and 100,000 output items; store values allow 256 Ki JSON characters each and 1 Mi characters total (keys included). There is no additional source, call-count, argument, or reply quota. `PI_CODEMODE_MAX_WORKERS` selects a positive integral process-wide worker limit (default 2), read on each admission; waiting is cancellable and lasts at most 60 seconds. The VM exposes no Node APIs, filesystem, network, timers, or module loading; use tools for I/O.

## Models

`models` reaches the model catalog and runs non-LLM models with the session's credentials: classifiers, which answer typed questions about JSON state and, for some models, images, and image models, which generate images. Chat models are listed but cannot be run from scripts. Use `models.getAvailableOfType(type)` to discover the configured classifier and image models. It checks current credentials/account filters without reloading the catalog, refreshing OAuth or running credential commands; availability does not guarantee a successful remote request.

```ts
type ModelType = "chat" | "image" | "classifier";

/** A catalog entry. `provider` and `id` identify it; other fields depend on the type. */
interface ModelInfo {
  type?: ModelType;
  provider: string;
  id: string;
  name: string;
  api: string;
  input: ("text" | "image")[];
  contextWindow?: number;
  [key: string]: unknown;
}

declare const models: {
  /** Every known model of a type, optionally for one provider. */
  getModelsOfType(type: ModelType, provider?: string): Promise<ModelInfo[]>;
  /** Models with current compatible credential configuration and account filters; no remote probe. */
  getAvailableOfType(type: ModelType, provider?: string): Promise<ModelInfo[]>;
  /** One catalog entry, or undefined. */
  getModelOfType(type: ModelType, provider: string, id: string): Promise<ModelInfo | undefined>;
  /** Answer `context.questions` about `context.state`; answers are in `result.answers` by question ID. */
  classify(model: ModelInfo, context: ClassifierContext): Promise<ClassifierResult>;
  /** Generate images from `context.input` text and image blocks; show `result.output` blocks with image(). Can take minutes. */
  generateImages(model: ModelInfo, context: ImagesContext): Promise<ImagesResult>;
};
```

`classify()` and `generateImages()` use only the `provider` and `id` of `model`, so `{ provider, id }` works as well. They do not throw on provider errors: check `stopReason` and `errorMessage`. At most four such calls run at once per script; more calls wait for a free slot, so `Promise.all()` over many items is fine. Their usage is added to the `codemode` tool result and counts toward the session cost.

Model IDs differ between providers, for example `typesafe/jev-latest` and `openrouter/typesafe/jev-1.13`. Use `models.getAvailableOfType(type)` to find the IDs that work with the current credentials.

### Classify

```ts
interface ClassifierContext {
  /** The data to classify. */
  state: Record<string, unknown>;
  /** Images judged together with `state`. Only models whose `input` includes "image" accept them. */
  images?: { type: "image"; data: string; mimeType: string }[];
  /** Questions by ID. One call answers all of them. */
  questions: Record<string, ClassifierQuestion>;
}

type ClassifierQuestion =
  /** Pick one label. `criteria` maps each label to what it means. */
  | { type: "choice"; instructions: string; criteria: Record<string, string> }
  /** Score on an ordered scale. `criteria` describes each level, lowest first. */
  | { type: "score"; instructions: string; criteria: string[] }
  /** Yes or no. */
  | { type: "bool"; instructions: string; criteria: { true: string; false: string } };

interface ClassifierResult {
  provider: string;
  model: string;
  /** Answers by question ID. */
  answers: Record<string, ClassifierAnswer>;
  usage?: ModelUsage;
  stopReason: "stop" | "error" | "aborted";
  errorMessage?: string;
}

type ClassifierAnswer =
  | { type: "choice"; choice: string; probabilities: Record<string, number>; confidence: number }
  /** `score` is the expected level index, from 0 to `criteria.length - 1`. */
  | { type: "score"; score: number; confidence: number }
  /** Probability of `true`. */
  | { type: "bool"; probability: number };

/** Token counts and cost in USD, when the service reports them. */
type ModelUsage = { input: number; output: number; totalTokens: number; cost: { total: number } };
```

Classify several items by calling `classify()` once per item. This script sorts feedback messages, for example ones a tool returned earlier in the script:

```js
const jev = await models.getModelOfType("classifier", "typesafe", "jev-latest");
const results = await Promise.all(
  messages.map((message) =>
    models.classify(jev, {
      state: { message },
      questions: {
        sentiment: {
          type: "choice",
          instructions: "How does the user feel about the product?",
          criteria: { positive: "Satisfied or happy", negative: "Unhappy or frustrated", neutral: "Neither" },
        },
        urgency: {
          type: "score",
          instructions: "How urgently does this need a reply?",
          criteria: ["no reply needed", "reply this week", "reply today"],
        },
      },
    }),
  ),
);
return results.map((result, i) =>
  result.stopReason === "stop"
    ? { message: messages[i], sentiment: result.answers.sentiment.choice, urgency: result.answers.urgency.score }
    : { message: messages[i], error: result.errorMessage },
);
```

Classifiers whose `input` includes `"image"` also judge images. `tools.read()` returns an image file as an image block that `images` accepts. Other classifiers return an error result when `images` is not empty.

```js
const luna = await models.getModelOfType("classifier", "openai", "gpt-6-luna");
const photo = await tools.read({ path: "screenshot.png" });
const result = await models.classify(luna, {
  state: { task: "Settings page redesign" },
  images: [photo],
  questions: {
    broken: {
      type: "bool",
      instructions: "Does the screenshot show a broken layout?",
      criteria: { true: "Overlapping, cut-off, or misaligned elements", false: "Clean layout" },
    },
  },
});
```

### Generate images

```ts
interface ImagesContext {
  /** The prompt as text blocks, plus image blocks to edit or use as references. */
  input: (TextBlock | ImageBlock)[];
}

interface ImagesResult {
  provider: string;
  model: string;
  /** Generated images, and text blocks for models that also return text. */
  output: (TextBlock | ImageBlock)[];
  usage?: ModelUsage;
  stopReason: "stop" | "error" | "aborted";
  errorMessage?: string;
}

type TextBlock = { type: "text"; text: string; textSignature?: string };
/** `data` is base64. */
type ImageBlock = { type: "image"; data: string; mimeType: string };
```

Show generated images with `image(block)`. Do not print `data` with `text()`, `console`, or `return`: it is large and the model cannot read it as text. `image()` also saves each image to a temp file and puts its path in the result, so a later turn can copy or move the file.

```js
// @options: {"timeout_ms": 300000}
const painter = await models.getModelOfType("image", "openrouter", "google/gemini-2.5-flash-image");
const result = await models.generateImages(painter, {
  input: [{ type: "text", text: "A red fox in the snow, watercolor" }],
});
if (result.stopReason !== "stop") return result.errorMessage;
for (const block of result.output) {
  if (block.type === "image") image(block);
  else text(block.text);
}
```
