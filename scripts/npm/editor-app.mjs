// Build the npm distribution of the downloadable editor from release binaries.
//
//   node scripts/npm/editor-app.mjs <binaries-dir> [--verify]
//
// `<binaries-dir>/<rust target>/catchlight-editor[.exe]` holds one binary per
// platform (any subset, for a local check). Each becomes
// `@catchlight/editor-<platform>-<arch>`, restricted by `os`/`cpu` so npm
// installs only the one that runs; `@catchlight/editor` is a small launcher
// that lists them all as optional dependencies and runs the installed one.
// Tarballs and `releases.json` (publish order, integrity) land in
// `dist/npm-app/`, which `publish.mjs` publishes after the libraries.
//
// `--verify` installs the launcher and this machine's platform package from
// the tarballs into a temporary project and runs `catchlight-editor --version`.
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = resolve(import.meta.dirname, '../..');
const [binaries, ...flags] = process.argv.slice(2);
if (!binaries || flags.some(f => f !== '--verify')) {
  throw new Error('Usage: node scripts/npm/editor-app.mjs <binaries-dir> [--verify]');
}
const version = execFileSync('python3', [resolve(root, 'scripts/release/check.py')], { encoding: 'utf8' }).trim();
// Rust target -> [process.platform, process.arch]; mirrors scripts/release/editor_app.py.
const targets = {
  'x86_64-unknown-linux-gnu': ['linux', 'x64'],
  'aarch64-unknown-linux-gnu': ['linux', 'arm64'],
  'aarch64-apple-darwin': ['darwin', 'arm64'],
  'x86_64-apple-darwin': ['darwin', 'x64'],
  'x86_64-pc-windows-msvc': ['win32', 'x64'],
};
const repository = { type: 'git', url: 'git+https://github.com/hyperlogue/catchlight.git' };
const notices = ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md'];
const output = resolve(root, 'dist/npm-app');
rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });

const platforms = [];
for (const [target, [os, cpu]] of Object.entries(targets)) {
  const exe = os === 'win32' ? 'catchlight-editor.exe' : 'catchlight-editor';
  const binary = resolve(binaries, target, exe);
  if (!existsSync(binary)) continue;
  const name = `@catchlight/editor-${os}-${cpu}`;
  const dir = resolve(output, `editor-${os}-${cpu}`);
  mkdirSync(resolve(dir, 'bin'), { recursive: true });
  cpSync(binary, resolve(dir, 'bin', exe));
  chmodSync(resolve(dir, 'bin', exe), 0o755);
  for (const file of notices) cpSync(resolve(root, file), resolve(dir, file));
  writeFileSync(resolve(dir, 'README.md'),
    `# ${name}\n\nThe \`catchlight-editor\` binary for ${os} ${cpu}. Install` +
    ' [`@catchlight/editor`](https://www.npmjs.com/package/@catchlight/editor) instead;' +
    ' it picks this package when it runs here.\n');
  writeFileSync(resolve(dir, 'package.json'), JSON.stringify({
    name, version,
    description: `The Catchlight editor binary for ${os} ${cpu}`,
    license: 'MIT OR Apache-2.0', repository,
    os: [os], cpu: [cpu],
    files: ['bin', 'README.md', ...notices],
    preferUnplugged: true,
  }, null, 2) + '\n');
  platforms.push({ name, dir });
}
if (platforms.length === 0) throw new Error(`No binaries under ${binaries}`);

const launcher = resolve(output, 'editor');
mkdirSync(resolve(launcher, 'bin'), { recursive: true });
cpSync(resolve(import.meta.dirname, 'editor-app/catchlight-editor.js'), resolve(launcher, 'bin/catchlight-editor.js'));
chmodSync(resolve(launcher, 'bin/catchlight-editor.js'), 0o755);
cpSync(resolve(import.meta.dirname, 'editor-app/README.md'), resolve(launcher, 'README.md'));
for (const file of notices) cpSync(resolve(root, file), resolve(launcher, file));
writeFileSync(resolve(launcher, 'package.json'), JSON.stringify({
  name: '@catchlight/editor', version,
  description: 'The Catchlight editor in your browser, drivable by local agents: run `npx @catchlight/editor`',
  license: 'MIT OR Apache-2.0', repository,
  homepage: 'https://github.com/hyperlogue/catchlight#readme',
  bin: { 'catchlight-editor': 'bin/catchlight-editor.js' },
  engines: { node: '>=18' },
  files: ['bin', 'README.md', ...notices],
  optionalDependencies: Object.fromEntries(
    Object.values(targets).map(([os, cpu]) => [`@catchlight/editor-${os}-${cpu}`, version])),
}, null, 2) + '\n');

// Platform packages first: the launcher's optional dependencies must resolve
// the moment it is published.
const releases = [];
for (const { name, dir } of [...platforms, { name: '@catchlight/editor', dir: launcher }]) {
  const [{ filename }] = JSON.parse(execFileSync('npm', ['pack', '--json', '--pack-destination', output], {
    cwd: dir, encoding: 'utf8',
  }));
  const integrity = `sha512-${createHash('sha512').update(readFileSync(resolve(output, filename))).digest('base64')}`;
  releases.push({ name, version, filename, integrity });
}
writeFileSync(resolve(output, 'releases.json'), JSON.stringify(releases, null, 2) + '\n');
console.log(`Packed: ${releases.map(r => r.filename).join(', ')}`);

if (flags.includes('--verify')) {
  const here = `@catchlight/editor-${process.platform}-${process.arch}`;
  const own = releases.find(r => r.name === here);
  if (!own) throw new Error(`No ${here} package to verify on this machine`);
  const main = releases.find(r => r.name === '@catchlight/editor');
  const dir = mkdtempSync(resolve(tmpdir(), 'catchlight-editor-npm-'));
  try {
    writeFileSync(resolve(dir, 'package.json'), JSON.stringify({ private: true }));
    // The launcher's other optional dependencies are not on any registry yet;
    // installing the two tarballs alone is what a user's install resolves to.
    execFileSync('npm', ['install', '--no-audit', '--no-fund', '--omit=optional',
      resolve(output, own.filename), resolve(output, main.filename)], { cwd: dir, stdio: 'inherit' });
    const shown = execFileSync(resolve(dir, 'node_modules/.bin/catchlight-editor'), ['--version'], { encoding: 'utf8' });
    if (shown.trim() !== `catchlight-editor ${version}`) throw new Error(`Unexpected version output: ${shown}`);
    console.log(`Verified ${main.filename} with ${own.filename}: ${shown.trim()}`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
