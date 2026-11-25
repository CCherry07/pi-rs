# Pinned rmcp HTTP error metadata compatibility patch

## Provenance

- Upstream: <https://github.com/modelcontextprotocol/rust-sdk>, package `rmcp` **3.3.0**.
- Source revision recorded by the published crate: `3e636cab26c013eca5131103c03d20237f12c4df`, `crates/rmcp`.
- Published `rmcp-3.3.0.crate` SHA-256: `b88db56b8ae316560e9e868b6b978ea940f27cb883323fc90e07435e44158f5c`.
- Extracted from that checksum-verified archive, not from edited Cargo registry sources.
- The original normalized manifest, upstream lockfile, tests and source are retained. The workspace
  excludes this directory as a member and selects it using `[patch.crates-io]`; the package version
  remains exactly 3.3.0. Upstream declares Apache-2.0 licensing. The standard `LICENSE-APACHE`
  text is included because the published archive omitted the license file; original notices remain.

## Approved boundary and reason

The unpatched reqwest POST backend turns non-success HTTP responses into
`UnexpectedServerResponse("HTTP ...: body")`, losing typed status and Retry-After. Pi's retry policy
cannot distinguish transient HTTP failures without parsing error strings. rmcp 3.3 has no public
response-injection/decode seam, and its bounded SSE decoder is private: an outer HTTP adapter would
have to resend requests or copy wire decoding. Neither is acceptable.

The approved compatibility patch is restricted to metadata at the existing **non-success POST
response** boundary and its transport error type. Preserve typed status and parsed Retry-After;
never expose arbitrary headers, credentials or unbounded opaque response bodies. Successful response
handling, serialization, SSE decoding, authentication exchanges, session recovery and SDK retry
policy must remain unchanged. Retry scheduling remains exclusively MCP product policy.

## Source delta

Only two upstream files change, recorded exactly in `PI-PATCH.diff`:

- `src/transport/streamable_http_client.rs`: additive `HttpStatus { status, retry_after }` variant.
  The existing error enum is non-exhaustive. Display includes only the status, not opaque bodies or
  arbitrary headers. Retry-After is typed as an optional duration, never retained as raw text.
- `src/transport/common/reqwest/streamable_http_client.rs`: return that variant at the existing
  non-success POST boundary, parse delta-seconds/HTTP-date Retry-After, and cap non-success JSON
  bodies at 16 KiB before the existing JSON-RPC-error/legacy-discovery interpretation. Oversized,
  invalid or opaque non-MCP bodies yield metadata only. Existing bounded JSON-RPC error payloads
  still use the original decoding path. The redirect test now asserts the more specific status.
  New regressions cover status/Retry-After, invalid/date values and oversized/omitted bodies.

401 challenge handling and session-expiry classification occur before this boundary and remain
untouched, as do successful JSON/SSE paths, OAuth code and session recovery. This is a transport
metadata/bounded-diagnostic change, not an alternative transport implementation. Product retries
use Pi's fixed 250/1000 ms schedule rather than an SDK retry loop; this patch does not itself sleep,
resend or decide whether a status is transient.

## Reproducible local checks

`python3 scripts/check-rmcp-patch.py` verifies the published archive checksum, all 174 upstream files,
the exact two-file delta, and the small allowlist of added documentation/license files. It reads a
matching local Cargo cache archive, or `--archive <path>`, and never downloads. Use `--write-diff`
only after deliberately reviewing a patch update.

```sh
cargo test --manifest-path vendor/rmcp-3.3.0/Cargo.toml --locked \
  --target-dir target/rmcp-compat-tests --lib \
  --features transport-streamable-http-client-reqwest,client,server,macros
cargo test -p pi-plugin-mcp --test lifecycle_retry
cargo test -p pi-plugin-mcp
```

The upstream-shaped `post_error_does_not_expose_unbounded_response_body` regression failed against
unmodified 3.3.0 before the implementation. The focused SDK HTTP tests and product lifecycle cases
now pass, including anonymous/provider/stored-OAuth equivalence, bounded retries, cancellation and
no permanent/stdio retry. All 262 standalone SDK library tests and the complete MCP package tests,
strict package Clippy, workspace Clippy/formatting and exact delta verification also pass. The
unfiltered workspace tests reproduce the confirmed clean-main codemode depth-512 stack overflow;
all other targets and the remaining 73 Coding integration cases pass. This is an explicit baseline
exception, not a fully green unfiltered workspace test result; see the [frozen MCP ledger](../../docs/mcp-post-auth-gap-matrix.md).
Optional strict
standalone SDK Clippy reports 15 unchanged upstream findings, reproduced on the checksum-verified
pristine archive: 13 `result_large_err` and two `too_many_arguments`. No lint allowances or unrelated
SDK changes were made; workspace Clippy remains strict. These local fixtures are **not official
conformance evidence** and make no full-parity claim.

## Removal criterion

Remove this patch and the workspace override when a pinned upstream release preserves the same typed
status/Retry-After boundary and bounded diagnostic contract. First run the upstream-shaped regression,
local anonymous/provider/OAuth retry and cancellation cases, JSON/SSE/session/auth compatibility cases,
and the product's pinned official conformance comparison; review differences rather than weakening
the baseline. Do not silently carry this patch onto another SDK version.
