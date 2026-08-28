You are working in a React 19 + TypeScript + Vite frontend fixture.

Project rules:
- Preserve strict TypeScript; do not introduce `any` unless an external boundary requires it.
- Prefer small React components, semantic HTML, and browser-native APIs.
- Keep keyboard navigation, accessible names, and visible focus behavior intact.
- Reuse the existing styling approach instead of adding a UI framework.
- Do not edit generated output or dependencies under `dist/` or `node_modules/`.
- After code changes, run `npm run lint` and `npm run build` and report failures clearly.
- Keep changes scoped to this fixture; do not modify the parent Rust workspace unless explicitly asked.
