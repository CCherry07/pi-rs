# React + TypeScript + Vite

This template provides a minimal setup to get React working in Vite with HMR and some Oxlint rules.

Currently, two official plugins are available:

- [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react) uses [Oxc](https://oxc.rs)
- [@vitejs/plugin-react-swc](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react-swc) uses [SWC](https://swc.rs/)

## React Compiler

The React Compiler is enabled on this template. See [this documentation](https://react.dev/learn/react-compiler) for more information.

Note: This will impact Vite dev & build performances.

## Expanding the Oxlint configuration

If you are developing a production application, we recommend enabling type-aware lint rules by installing `oxlint-tsgolint` and editing `.oxlintrc.json`:

```json
{
  "$schema": "./node_modules/oxlint/configuration_schema.json",
  "plugins": ["react", "typescript", "oxc"],
  "options": {
    "typeAware": true
  },
  "rules": {
    "react/rules-of-hooks": "error",
    "react/only-export-components": ["warn", { "allowConstantExport": true }]
  }
}
```

See the [Oxlint rules documentation](https://oxc.rs/docs/guide/usage/linter/rules) for the full list of rules and categories.

## Native pi plugin

This fixture includes a project-local Rust AgentPlugin under
`pi-plugins/frontend-workflow`. Build it before starting `pi` in this directory:

```bash
cargo test --manifest-path pi-plugins/frontend-workflow/Cargo.toml
cargo build --manifest-path pi-plugins/frontend-workflow/Cargo.toml
```

After project trust is approved, `pi` discovers its manifest automatically. The plugin registers
`/frontend-check [focus]` and injects the configured lint/build requirements at generation time.

## TypeScript pi extension

`.pi/extensions/frontend-napi.ts` exercises the Node/NAPI extension host. From the `pi_rs`
workspace root, build and launch the Node package with this project as its working directory:

```bash
cd packages/pi
npm install
npm run build:native
npm start -- --cwd ../../e2e/projects/frontend-app --approve
```

The extension is discovered automatically after project trust is approved. It registers the
`frontend_project_checks` tool and `/frontend-napi-smoke`, which transforms into a real agent
prompt and therefore enters the normal `Working` flow. It also contributes agent, provider, and
session lifecycle hooks. Editing it followed by `/reload` creates a fresh callback generation.
