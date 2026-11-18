# Models and providers

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


Use [models.json](models.md) for compatible HTTP routes and catalog entries. Executable provider hooks are described in [extensions](extensions.md); Rust embedders use the [SDK](sdk.md).
