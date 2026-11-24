# Models and request-time credentials

`pi-plugin-models` owns the `models.json` catalog and custom-provider routing. It reuses the
existing protocol providers; a compatible gateway does not need a new Agent loop or a dedicated
provider implementation. See the [product model configuration guide](../../../README.md#model-configuration).

## Command-backed credentials

An `apiKey` or header value beginning with `!` runs the remaining text as a shell command when
that value is needed for a request. The command's trimmed stdout is the value. The helper must
exit successfully and produce nonempty output; a JSON credential envelope is not decoded into a
token automatically.

- Commands run again on each resolution, including later requests in the same session/generation.
  Successful output and failures are not cached. Each configured command reference is resolved
  independently; prefer a single `apiKey` reference for a Bearer-authenticated endpoint.
- Loading/validating `models.json` and checking configured model availability do not execute
  helpers. An actual catalog request, like an inference request, resolves its credentials.
- An explicit runtime API key takes precedence over the configured `apiKey` command. Configured
  header commands still apply separately.
- Execution uses `$SHELL -c` (falling back to `/bin/sh -c`), with no stdin, a ten-second timeout,
  and cancellation support. Helpers must work non-interactively.
- Failures stop the request before model HTTP I/O rather than falling back to an earlier token.
  Errors identify the configuration field but omit the command text, stdout and stderr, which
  may contain secrets. This diagnostic redaction is an intentional Rust safety difference.
- Resolved credentials are not added to the public model catalog or persisted by this resolver.
  The external helper may maintain its own login/refresh state.

This follows current Pi's request-time `resolveConfigValueOrThrow` / uncached command path in
`legacy/pi/packages/coding-agent/src/core/{provider-composer,resolve-config-value}.ts`. It is not
a background refresh loop or a Codex external-auth protocol implementation. There is no automatic
401 refresh-and-retry here: the next request resolves the command again.

Only configure commands you trust. Do not embed tokens in command arguments or configure helpers
that print diagnostics to stdout. Use a trusted wrapper if a helper needs to transform structured
output into a raw credential, and keep its diagnostics free of secrets.

## Catalog and availability

Coding enables the complete specialized catalog at pinned legacy revision
`0ab23c37b074252d04e31f5cfb99491f0613fb80`: 46 classifier and 63 image models across Cloudflare
Workers AI, OpenAI, OpenCode, OpenRouter, TypeSafe and Vercel AI Gateway. The offline generator
`scripts/sync-specialized-model-catalog.py` supports `--check`; normal builds need neither Node nor
a legacy checkout. Typed entries preserve input limits, costs, headers, classifier context/compat
and image output modalities, and do not replace chat models of the same provider/id.

`ModelRuntime::query_available_models_of_type` checks current provider credentials and account
filters without changing the generation catalog. Coding's stored-credential checks reread the
auth file without refreshing OAuth, creating auth locks or executing command-backed keys. Commands
count as configured; environment templates require their variables to be present. Availability is
a local configuration check, not proof that a remote account can successfully call a model.
Existing synchronous catalog APIs continue to expose a generation-time snapshot.

Cloudflare specialized requests resolve account placeholders from current credential environment
metadata, falling back to ambient environment; placeholder substitutions remain individual URL
path segments. OpenAI Decisions excludes OAuth-only credentials. Copilot filters apply to current
account metadata without removing models from the registered catalog.

## Configuration notes

Set `contextWindow`, `maxTokens`, input capabilities, thinking and compatibility settings to match
the configured model's documented behavior. Keep existing providers while validating the new route. The
Responses provider sends the resolved `apiKey` as `Authorization: Bearer ...`; do not add a second
command-backed `Authorization` header or copy a token into the file.

Adding or changing the helper command requires reload; changes to the credential it returns do
not. A helper's successful execution alone does not prove gateway access, model permissions,
streaming, or tool-call compatibility. Validate those separately against your intended endpoint.

## Tests

```bash
cargo test -p pi-plugin-models
cargo clippy -p pi-plugin-models --all-targets -- -D warnings
```

`tests/command_credentials.rs` uses local shell fixtures and an in-process capturing HTTP transport.
It covers lazy resolution, key/provider-header/model-header rotation, recovery after helper
failure, separation of catalog and inference credentials, explicit runtime-key precedence, and
request-time credentials combined with session-derived gateway thread identity.
Resolver tests cover empty output, redaction, cancellation and timeout. Shell execution tests are
Unix-gated; they do not invoke external credential tools, read real credentials, or send network requests.
