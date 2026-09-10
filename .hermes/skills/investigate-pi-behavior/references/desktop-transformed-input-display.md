# Desktop transformed-input display tracing

Use this when a registered command (especially `/skill:<name>`) expands into model-facing text but Desktop should continue to show the submitted command.

## Data identities

Keep these distinct:

- **display text**: the original user submission, e.g. `/skill:performance-analysis ...`;
- **model text**: the expanded prompt, which may contain skill contents and absolute locations;
- **semantic rendering**: Markdown may reinterpret display text as a file path even when the backend projection is correct.

## Trace

1. Confirm command transformation captures the original submission before replacing model text in `crates/pi-runtime/src/lib.rs`.
2. Confirm `crates/pi-session/src/agent_session.rs` persists the model-facing `Message::User` as an `AgentMessage` annotated with `piRs.displayText`, and projects live user events using display text.
3. Inspect both Desktop paths:
   - live `MessageStart` followed by `EntryAppended` in `apps/pi-desktop/src-tauri/src/pi_runtime/projection.rs`;
   - snapshot/resume and stored-isolated projection in `apps/pi-desktop/src-tauri/src/pi_runtime/mod.rs`.
4. Never reduce the persisted `AgentMessage` to only `Message` before reading `display_text()`. The standard message contains model text; metadata contains display text.
5. Ensure the later `EntryAppended` update uses display text. React upsert/merge policy can otherwise replace a correct live item with longer expanded text.
6. Check session-list title/preview derivation in `session_store.rs`; these must also prefer display text.
7. Render the final display text through the real Markdown component. Slash commands beginning with `/` can be mistaken for absolute paths, producing bogus relative parents such as `../../..`.

## Regression coverage

- A transforming command persists expanded model text but projects only the original slash command in the message, preview, and session title.
- The `EntryAppended` projection prefers persisted display text and preserves image blocks.
- A Markdown test renders `/skill:name arguments` without a `.message-file-link` and without synthetic relative-path text.
- Verify live, settled, resume, and stored-child paths rather than testing only initial optimistic rendering.
