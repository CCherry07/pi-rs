# Desktop appearance

Pi Desktop uses a quiet three-pane workspace: compact project navigation, an open
conversation canvas, and a secondary file/plan panel. This is a deliberate Desktop
presentation design, not a claim of visual parity with the legacy Pi terminal themes.
It does not change session behavior, IPC, or provider execution.

## Color

The light palette follows the supplied Codex theme reference: white surface,
`#1a1c1f` ink, `#339cff` accent, and translucent windows. Dark uses a graphite
`#1a1c1f` canvas with the same blue accent. Dim and system preferences remain available.

| Role | Light | Dark |
| --- | --- | --- |
| Conversation canvas | `#ffffff` | `#1a1c1f` |
| Sidebar, opaque equivalent | `#f4f5f7` | `#16181b` |
| Main text | `#1a1c1f` | `#e6e8ec` |
| Accent fill | `#339cff` | `#339cff` |
| Links / accent text | `#006fc4` | `#75b9ff` |
| Selected navigation row | `#e4effa` | `#25384a` |
| Diff added / removed accents | `#00a240` / `#ba2623` | Same base hues |
| Skill accent | `#924ff7` | Same base hue |

The primary blue uses dark text (`#102334`) for legibility. Small semantic labels
use separate theme-specific text colors. Text, links, primary controls, and sampled
diff/skill labels exceed 4.5:1 contrast against their intended opaque surfaces.
Decorative and disabled colors are separate. Native sidebar transparency remains
available; Reduce transparency uses the opaque sidebar token.

The supplied `codex-theme-v1` string is a design reference, not an imported runtime
configuration. Its `contrast` and `codeThemeId` fields are not new Pi settings.
Syntax highlighting retains the existing Prism/Pierre engines; Prism colors use the
desktop tokens. Existing user font settings keep their precedence.

## Typography and components

- System UI fonts for navigation, prose, and input. Assistant prose is 15 px with
  1.75 line height; user messages and the composer are 14 px. Code uses the existing
  configured monospace font, with a 12 px CSS fallback.
- The conversation column is at most 800 px. Fluid horizontal padding and 24 px
  message spacing keep the reading rhythm consistent.
- Assistant prose sits directly on the canvas. User messages use a muted bubble.
  Tools keep their existing expand/collapse interaction and semantic status.
- Controls use 6–10 px corners; the composer uses 16 px with a thin border and
  restrained shadow. Focus, hover, selection, error, and disabled states are distinct.
- Panels use thin dividers. Latest-task cards have a flat surface and a subtle hover
  state. Plan steps use check/circle icons, retaining their existing status data.
- Reduced-motion support remains in the shared stylesheet.

## Implementation and preview

`src/styles/app.css` is the shared production/preview stylesheet entry. Theme values
live in `themes.dark.css`, `themes.light.css`, `themes.dim.css`, and
`themes.system.css`; system light must match explicit light.

From `apps/pi-desktop`, run:

```sh
npm run dev -- --host 127.0.0.1
```

Open <http://127.0.0.1:1421/examples/design-preview.html?theme=light> or change the
query to `dark`, `dim`, or `system`. The toolbar switches between conversation,
new-task, and appearance-spec views. Search, local sample messages, model selection,
tool expansion, file diffs, theme selection, and transparency can be exercised.

The preview reuses production React components and CSS. It supplies fixture data,
mocks Tauri event registration, and rejects native commands. External app launch
controls are omitted. Sending a preview message produces a local sample reply;
no model is called and no project is changed. Preview framing collapses auxiliary
panes in narrow browser panels; production keeps its existing responsive layout.
The example entry is not part of the production bundle.

Validate frontend changes with `npm run typecheck`, `npm run lint`, `npm test`, and
`npm run build`, plus light/dark visual checks. A browser preview does not verify
the installed Tauri binary or operating-system vibrancy.
