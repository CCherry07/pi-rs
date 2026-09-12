// Local author build: one ESM module + CSS, sharing the desktop's React runtime.
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';
import * as React from 'react';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const packageRoot = resolve(process.argv[2] ?? '');
if (!process.argv[2]) throw new Error('Usage: npm run build:extension -- <plugin-directory>');
const manifest = JSON.parse(await readFile(resolve(packageRoot, 'pi-desktop.json'), 'utf8'));
if (manifest.schemaVersion !== 1 || !manifest.id) throw new Error('Expected a schemaVersion: 1 desktop manifest');
const sharedNames = ['react', 'react-dom', 'react-dom/client', 'react/jsx-runtime', 'react/jsx-dev-runtime'];
const shared = new Map(await Promise.all(sharedNames.map(async name => [name, Object.keys(await import(name))])));
const prefix = '\0pi-desktop-shared:';
const sdk = resolve(desktop, '../../packages/pi-desktop-sdk/index.mjs');
await build({
  configFile: false,
  root: desktop,
  publicDir: false,
  plugins: [{
    name: 'pi-desktop-shared-runtime',
    resolveId(id) {
      if (shared.has(id)) return prefix + id;
      if (id === '@pi-rs/desktop-sdk') return sdk;
    },
    load(id) {
      if (!id.startsWith(prefix)) return;
      const name = id.slice(prefix.length);
      const exports = shared.get(name).filter(key => key !== 'default' && /^[a-zA-Z_$][\w$]*$/.test(key));
      return `const m=globalThis[Symbol.for('pi.desktop.runtime.v1')].modules[${JSON.stringify(name)}];\n`
        + `export default m.default ?? m;\n`
        + exports.map(key => `export const ${key}=m.${key};`).join('\n');
    },
  }],
  build: {
    outDir: resolve(packageRoot, 'dist'), emptyOutDir: true, cssCodeSplit: false,
    lib: { entry: resolve(packageRoot, 'src/index.tsx'), formats: ['es'], fileName: () => 'index.js', cssFileName: 'style' },
    rolldownOptions: { output: {
      codeSplitting: false,
      banner: `if(globalThis[Symbol.for('pi.desktop.runtime.v1')]?.modules.react.version!==${JSON.stringify(React.version)})throw new Error('Desktop React version changed; rebuild this extension');`,
    } },
  },
});
const { existsSync } = await import('node:fs');
const output = {schemaVersion: 1, id:manifest.id, name:manifest.name, entry:'index.js', styles:existsSync(resolve(packageRoot,'dist/style.css')) ? ['style.css'] : []};
await mkdir(resolve(packageRoot, 'dist'), {recursive:true});
await writeFile(resolve(packageRoot, 'dist/pi-desktop.json'), JSON.stringify(output, null, 2)+'\n');
