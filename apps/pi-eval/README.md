# pi-eval

`pi-eval` runs model-backed checks through the same headless `Pi` product
composition used by the CLI and desktop application. Each run gets isolated
workspace, agent, and session directories. Only `auth.json` and `models.json`
are copied into the temporary agent directory for provider bootstrap; they are
never attached to eval artifacts.

This app owns argument parsing, the first-party case catalog, and result
presentation. [`pi-coding-eval`](../../domains/coding/eval/README.md) owns the
Coding-specific configuration and environment preparation.
[`pi-eval`](../../crates/pi-eval/README.md) owns the reusable session runner,
observations, graders, artifacts, and comparisons. Other domains can use that
runner by preparing their own session manager through `pi-sdk::AgentHost`.

The native-only smoke case can run through Cargo:

```bash
cargo run --bin pi-eval -- run smoke \
  --provider openai \
  --model gpt-5.6-sol
```

JavaScript/TypeScript extension cases use the Node/NAPI launcher so Jiti and
the callback host participate in every runtime generation:

```bash
./scripts/pi-eval-dev run coding/js-extension \
  --provider openai \
  --model gpt-5.6-sol
```

The first-party catalog also contains:

- `docs`: audits the explicit product-documentation manifest against source.
- `docs/<path>`: runs one manifest entry, such as `docs/README.md`.
- `coding/js-extension`: creates, reloads, and invokes a TypeScript tool using
  baseline and candidate system prompts.
- `coding/model`: adds a model to the existing `openai` catalog, reloads the
  generation, and validates the resulting model metadata without calling it.
- `coding/provider`: adds an OpenAI Chat Completions provider, reloads the
  generation, and verifies catalog metadata, authentication, wire format,
  streaming response, and usage against an invocation-local fixture server.
- `provider/native-plugin`: builds and loads a test-only native custom-provider
  fixture through the product plugin loader, reloads it, and verifies
  multi-delta streaming, usage accounting, and stream-error propagation.
- `coding/native-plugin`: creates and compiles a version-locked Rust `cdylib`,
  discovers its project `pi-plugin.toml` on reload, and invokes its tool.

`--extension` adds an explicit JS/TS source and requires the Node launcher.
`--native-plugin` accepts a native library, `pi-plugin.toml`, or package
directory and may be repeated. Project plugins created under `.pi/plugins`
are discovered by the normal trusted-project product loader after reload.

`PI_PROVIDER` and `PI_MODEL` can supply the defaults. `--repetitions` repeats
the case and `.eval/<invocation>/runs.jsonl` records every observation. Native
Pi session JSONL, the effective system prompt, normalized transcript data,
grades, and workspace changes are stored under the corresponding
`runs/<run-id>/` directory.

Comparative cases run their declared baseline and candidates for the same case
and repetition. They write `report.json` and `report.txt` with paired pass-rate
lift and candidate-minus-baseline token, latency, and estimated-cost deltas.
Harness errors and missing scores remain diagnostics; they are never coerced
to correctness failures or zero telemetry. Use `--variant` to run one treatment
while developing without producing an incomplete comparison report.

The smoke case disables tools and requires the final response to be exactly
`Paris` after trimming. Model-backed evals are intentionally separate from
deterministic crate tests and are not part of the default workspace test gate.

The temporary workspace is isolation from user configuration, not an operating
system sandbox. Documentation cases intentionally read the source checkout,
and coding cases enable filesystem or shell tools. Run them in a container or
VM when model access to the host would be unsafe.
