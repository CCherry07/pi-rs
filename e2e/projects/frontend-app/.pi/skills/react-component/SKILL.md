---
name: react-component
description: Implement or refactor React components in this Vite fixture while preserving strict TypeScript, accessibility, and the existing visual language.
---

# React component workflow

1. Read `src/App.tsx`, the relevant CSS, and any nearby component before editing.
2. Keep state local unless multiple components genuinely need to coordinate.
3. Use semantic elements before adding ARIA. Every interactive control needs an accessible name.
4. Avoid `any`, non-null assertions without justification, and effects for values that can be derived during render.
5. Preserve the React Compiler-compatible style: render must remain pure and props/state must not be mutated.
6. Run `npm run lint` and `npm run build` after changes.
