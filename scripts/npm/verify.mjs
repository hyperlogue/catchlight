// Install the actual tarballs outside the workspace: no source aliases or Bun
// loader may hide missing JS, declarations, peers, or packaged assets.
import { mkdtempSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

const root = resolve(import.meta.dirname, '../..');
const output = resolve(root, 'dist/npm');
const releases = JSON.parse(readFileSync(resolve(output, 'releases.json'), 'utf8'));
const dir = mkdtempSync(resolve(tmpdir(), 'catchlight-npm-'));
try {
  writeFileSync(resolve(dir, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund',
    ...releases.map(p => resolve(output, p.filename)), 'react@19', 'react-dom@19', '@types/react@19', '@types/react-dom@19'], { cwd: dir, stdio: 'inherit' });
  for (const { name } of releases) {
    const pkgDir = resolve(dir, 'node_modules', name);
    const pkg = JSON.parse(readFileSync(resolve(pkgDir, 'package.json'), 'utf8'));
    assert(!JSON.stringify(pkg).includes('workspace:'));
    for (const file of ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md']) assert(existsSync(resolve(pkgDir, file)), `${name}/${file}`);
    for (const target of Object.values(pkg.exports)) {
      for (const file of typeof target === 'string' ? [target] : Object.values(target)) assert(existsSync(resolve(pkgDir, file)), file);
    }
  }
  assert(existsSync(resolve(dir, 'node_modules/@catchlight/editor/dist/logo.svg')));
  writeFileSync(resolve(dir, 'smoke.mjs'), `
import assert from 'node:assert/strict';
import { Editor, InTabBackend, MemoryStorage } from '@catchlight/core';
import { fakeWasm } from '@catchlight/core/fakes';
import { EditorProvider } from '@catchlight/react';
import { CatchlightEditor, themeStylesheet } from '@catchlight/editor';
assert.equal(typeof Editor, 'function');
assert.equal(typeof InTabBackend, 'function');
assert.equal(typeof MemoryStorage, 'function');
assert.equal(typeof fakeWasm, 'function');
assert.equal(typeof EditorProvider, 'function');
assert.equal(typeof CatchlightEditor, 'function');
assert.equal(themeStylesheet, '@catchlight/editor/theme.css');
`);
  execFileSync(process.execPath, ['smoke.mjs'], { cwd: dir, stdio: 'inherit' });
  writeFileSync(resolve(dir, 'smoke.ts'), `
import { Editor, type WasmModule } from '@catchlight/core';
import { EditorProvider } from '@catchlight/react';
import { CatchlightEditor, type CatchlightEditorProps } from '@catchlight/editor';
export const component: (props: CatchlightEditorProps) => React.JSX.Element = CatchlightEditor;
export type Host = { editor: Editor; wasm: WasmModule; provider: typeof EditorProvider };
`);
  execFileSync(process.execPath, [resolve(root, 'node_modules/typescript/bin/tsc'), '--noEmit', '--strict', '--module', 'NodeNext', '--moduleResolution', 'NodeNext', '--target', 'ES2022', '--lib', 'ES2022,DOM,DOM.Iterable,ESNext.Disposable', 'smoke.ts'], { cwd: dir, stdio: 'inherit' });
  console.log('Packed npm packages: Node imports, consumer types, exports, assets and notices verified');
} finally { rmSync(dir, { recursive: true, force: true }); }
