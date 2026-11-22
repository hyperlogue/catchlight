// Publish only the reviewed tarballs, in dependency order; resume partial runs.
//
//   node scripts/npm/publish.mjs [--publish] [--with-app]
//
// The libraries come from `dist/npm` (`npm:pack`). `--with-app` also requires
// the downloadable editor's packages from `dist/npm-app` (`editor-app.mjs`):
// all five platform packages, then the `@catchlight/editor` launcher.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const version = execFileSync('python3', [resolve(import.meta.dirname, '../release/check.py')], { encoding: 'utf8' }).trim();
const flags = process.argv.slice(2);
if (flags.some(f => f !== '--publish' && f !== '--with-app')) throw new Error('Usage: node scripts/npm/publish.mjs [--publish] [--with-app]');
const live = flags.includes('--publish');
const sets = [{ dir: resolve(import.meta.dirname, '../../dist/npm'),
  expected: ['@catchlight/core', '@catchlight/react', '@catchlight/editor-ui'] }];
if (flags.includes('--with-app')) {
  sets.push({ dir: resolve(import.meta.dirname, '../../dist/npm-app'),
    expected: ['linux-x64', 'linux-arm64', 'darwin-arm64', 'darwin-x64', 'win32-x64']
      .map(p => `@catchlight/editor-${p}`).concat('@catchlight/editor') });
}
const releases = [];
for (const { dir, expected } of sets) {
  const set = JSON.parse(readFileSync(resolve(dir, 'releases.json'), 'utf8'));
  if (JSON.stringify(set.map(p => p.name)) !== JSON.stringify(expected)) throw new Error(`Unexpected npm release set/order in ${dir}`);
  releases.push(...set.map(pkg => ({ ...pkg, dir })));
}
for (const pkg of releases) {
  if (pkg.version !== version) throw new Error(`Archive version must match the shared release ${version}`);
  if (pkg.filename !== `catchlight-${pkg.name.split('/')[1]}-${pkg.version}.tgz`) throw new Error('Unexpected archive name');
  const file = resolve(pkg.dir, pkg.filename);
  const integrity = `sha512-${createHash('sha512').update(readFileSync(file)).digest('base64')}`;
  if (integrity !== pkg.integrity) throw new Error(`Archive changed: ${pkg.name}`);
}
for (const pkg of releases) {
  const response = await fetch(`https://registry.npmjs.org/${encodeURIComponent(pkg.name)}/${encodeURIComponent(pkg.version)}`, { signal: AbortSignal.timeout(30_000) });
  if (response.status === 200) { console.log(`Already published: ${pkg.name}@${pkg.version}`); continue; }
  if (response.status !== 404) throw new Error(`Registry lookup failed: ${pkg.name} HTTP ${response.status}`);
  execFileSync('npm', ['publish', resolve(pkg.dir, pkg.filename), '--access', 'public', '--registry', 'https://registry.npmjs.org', '--ignore-scripts', ...(live ? [] : ['--dry-run'])], { stdio: 'inherit' });
}
