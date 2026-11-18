# Themes

The terminal frontend owns colors and styling. Extensions use `ctx.ui.theme` and the supported
text/UI compatibility API; read the [TUI contract](tui.md) before implementing a renderer.
The `theme` setting selects the frontend theme and is preserved when other settings change.
Use [extension registration](extensions.md) for custom renderers. Frontend support and deliberate
compatibility limits are described by the TUI contract.
