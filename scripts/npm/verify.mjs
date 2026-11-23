// Install the actual tarballs outside the workspace: no source aliases or Bun
// loader may hide missing JS, declarations, peers, or packaged assets.
import { mkdtempSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

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
  assert(existsSync(resolve(dir, 'node_modules/@catchlight/editor-ui/dist/logo.svg')));
  for (const name of ['react', 'editor-ui']) {
    assert(!existsSync(resolve(dir, `node_modules/@catchlight/${name}/dist/wasm`)), 'Only core ships WASM');
  }
  writeFileSync(resolve(dir, 'smoke.mjs'), `
import assert from 'node:assert/strict';
import { Editor, InTabBackend, MemoryStorage } from '@catchlight/core';
import { fakeWasm } from '@catchlight/core/fakes';
import { EditorProvider } from '@catchlight/react';
import { CatchlightEditor, themeStylesheet } from '@catchlight/editor-ui';
assert.equal(typeof Editor, 'function');
assert.equal(typeof InTabBackend, 'function');
assert.equal(typeof MemoryStorage, 'function');
assert.equal(typeof fakeWasm, 'function');
assert.equal(typeof EditorProvider, 'function');
assert.equal(typeof CatchlightEditor, 'function');
assert.equal(themeStylesheet, '@catchlight/editor-ui/theme.css');
`);
  execFileSync(process.execPath, ['smoke.mjs'], { cwd: dir, stdio: 'inherit' });
  writeFileSync(resolve(dir, 'smoke.ts'), `
import { Editor, InTabBackend, MemoryStorage, type WasmModule } from '@catchlight/core';
import init, * as wasm from '@catchlight/core/wasm';
import { EditorProvider } from '@catchlight/react';
import { CatchlightEditor, type CatchlightEditorProps } from '@catchlight/editor-ui';
export const component: (props: CatchlightEditorProps) => React.JSX.Element = CatchlightEditor;
export type Host = { editor: Editor; wasm: WasmModule; provider: typeof EditorProvider };
export async function createEditor() {
  await init();
  return Editor.create(wasm, new InTabBackend(new wasm.CatchlightEditor(), new MemoryStorage()));
}
`);
  execFileSync(process.execPath, [resolve(root, 'node_modules/typescript/bin/tsc'), '--noEmit', '--strict', '--module', 'NodeNext', '--moduleResolution', 'NodeNext', '--target', 'ES2022', '--lib', 'ES2022,DOM,DOM.Iterable,ESNext.Disposable', 'smoke.ts'], { cwd: dir, stdio: 'inherit' });
  // Catches generated loader URLs that work in Node but break when a bundler moves JS.
  writeFileSync(resolve(dir, 'index.html'), '<script type="module" src="/browser.js"></script>');
  writeFileSync(resolve(dir, 'browser.js'), `
import { Editor, InTabBackend, MemoryStorage } from '@catchlight/core';
import init, * as wasm from '@catchlight/core/wasm';
window.start = async () => {
  await init();
  const editor = await Editor.create(wasm, new InTabBackend(new wasm.CatchlightEditor(), new MemoryStorage()));
  try {
    await editor.newSession('npm consumer');
    return (await editor.listSessions()).map(session => session.title);
  } finally { editor.close(); }
};
`);
  const require = createRequire(resolve(root, 'apps/site/package.json'));
  const { build, preview } = await import(require.resolve('vite'));
  const { chromium } = require('playwright-core');
  await build({ root: dir, configFile: false, envDir: false, logLevel: 'warn', build: { target: 'es2022' } });
  const server = await preview({ root: dir, configFile: false, envDir: false, logLevel: 'warn', preview: { host: '127.0.0.1', port: 0 } });
  let browser;
  try {
    const executablePath = process.env.CHROMIUM ?? execFileSync('which', ['chromium'], { encoding: 'utf8' }).trim();
    browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu'] });
    const page = await browser.newPage();
    const requests = [];
    page.on('request', request => { if (new URL(request.url()).pathname.endsWith('.wasm')) requests.push(request.url()); });
    await page.goto(server.resolvedUrls.local[0]);
    await page.waitForFunction(() => typeof window.start === 'function');
    assert.equal(requests.length, 0, 'Importing the WASM subpath must not fetch or initialize it');
    assert.deepEqual(await page.evaluate(() => window.start()), ['npm consumer']);
    assert.equal(requests.length, 1, 'Explicit initialization loads the packaged WASM asset');
    assert.deepEqual(await page.evaluate(() => window.start()), ['npm consumer']);
    assert.equal(requests.length, 1, 'Repeated initialization reuses the loaded module');
  } finally {
    await browser?.close();
    await new Promise((done, reject) => server.httpServer.close(error => error ? reject(error) : done()));
  }
  console.log('Packed npm packages: Node imports, consumer types, assets, notices and browser WASM initialization verified');
} finally { rmSync(dir, { recursive: true, force: true }); }
