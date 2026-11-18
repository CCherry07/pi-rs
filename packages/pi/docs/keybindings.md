# Interactive workflow

Curator maintains global Hermes skills and skills managed by the memory plugin under the current
trusted Git checkout's `.hermes/skills`. Inspect both with `/curator status`; preview maintenance
with `/curator run --dry-run`. Select one library with `--scope global` or `--scope project`:

```text
/curator run --scope project --dry-run
/curator run --scope project --consolidate
/curator adopt project:my-repo:release
/curator pin project:my-repo:release
/curator list-archived --scope project
/curator rollback --scope project --list
/curator rollback --scope project
```

The standalone `pi curator ...` accepts the same arguments and uses the existing project-trust
policy. Local maintenance needs no model. Bare skill names and rollback default to global;
status/run default to both available scopes. Archive and backup IDs for projects include their
scope so they can be passed directly to `restore` or `rollback --id`. Each library has independent
pause state, archives and backups; consolidation stays inside one scope. Handwritten, pinned or
externally edited skills are protected unless explicitly adopted/unpinned as applicable.

Attach image files at startup using `@file` in interactive, print or JSON mode:

```bash
pi @screenshot.png "Explain this error"
pi --print @before.png @after.png "Compare these screenshots"
pi --json '@screen shot.png'
printf 'Context:\n' | pi --print @screenshot.png "Investigate"
```

PNG, JPEG, GIF, WebP and BMP are detected from file contents. Images normally fit within
2000×2000 and the inline encoded-size limit; BMP is converted to PNG. `images.autoResize: false`
in the effective settings disables resizing. `images.blockImages` retains its provider-input
filtering behavior. Text files retain Pi's `<file>` markup and BOM handling, and piped text is
combined before file references and the prompt. Empty files are skipped; malformed detected
images produce visible omission notices.

In the TUI, `Ctrl+V` reads an image or text from the local system clipboard. Following current Pi,
an image is saved as a temporary PNG and its path is inserted into the composer for the agent to
read. These files remain after exit so saved path references keep working. Terminal drag/drop
and normal paste can also insert file paths. Native clipboard reads depend on the OS/session and
are unavailable over SSH; use terminal text/path paste or startup `@file` there. Audio and video
attachments are not implemented yet.

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
| `/session`                    | Show session details, usage, and the latest request’s estimated context breakdown                     |
| `/reload`                     | Atomically rebuild plugins, models, resources, and session extensions |
| `/features [global\|project]` | Select optional capabilities, save, and reload the current session    |
| `/trust`                      | Review or change trust for the current project                        |
| `/copy`                       | Copy the last completed assistant response                            |
| `/clear`                      | Clear the visible transcript without deleting the session             |
| `/help`                       | Show the command list                                                 |
| `/quit`                       | Exit the application                                                  |
| `/skill:<name>`               | Invoke a discovered skill                                             |

Thinking follows Pi's ordered `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` scale.
New product sessions default to `medium`; selectors show only levels supported by the active model,
and an explicitly requested unsupported level clamps upward first, then downward.

Plugin-provided commands are added to the same command palette as built-in commands and skills.
Submitting a slash command does not show `Working...` while it opens a selector or changes settings.
If the command starts an agent turn, including skill or prompt-template expansion, runtime events
start the normal working indicator. Ordinary prompts keep immediate submission feedback.

Key bindings:

| Key                      | Action                                                                 |
| ------------------------ | ---------------------------------------------------------------------- |
| `Enter`                  | Complete a selected command, submit while idle, or steer while running |
| `Alt+Enter`              | Queue a follow-up message                                              |
| `Ctrl+J`                 | Insert a newline                                                       |
| `Ctrl+V`                 | Paste a local clipboard image path or text                             |
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


See the [TUI API](tui.md) for extension key handling and [CLI guide](../README.md) for commands.
