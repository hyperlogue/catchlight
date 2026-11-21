// Publish only the reviewed tarballs, in dependency order; resume partial runs.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const version = execFileSync('python3', [resolve(import.meta.dirname, '../release/check.py')], { encoding: 'utf8' }).trim();
const dir = resolve(import.meta.dirname, '../../dist/npm');
const releases = JSON.parse(readFileSync(resolve(dir, 'releases.json'), 'utf8'));
const expected = ['@catchlight/core', '@catchlight/react', '@catchlight/editor'];
if (JSON.stringify(releases.map(p => p.name)) !== JSON.stringify(expected)) throw new Error('Unexpected npm release set/order');
const live = process.argv[2] === '--publish';
if (process.argv.length > 2 && !live) throw new Error('Usage: node scripts/npm/publish.mjs [--publish]');
for (const pkg of releases) {
  if (pkg.version !== version) throw new Error(`Archive version must match the shared release ${version}`);
  if (pkg.filename !== `catchlight-${pkg.name.split('/')[1]}-${pkg.version}.tgz`) throw new Error('Unexpected archive name');
  const file = resolve(dir, pkg.filename);
  const integrity = `sha512-${createHash('sha512').update(readFileSync(file)).digest('base64')}`;
  if (integrity !== pkg.integrity) throw new Error(`Archive changed: ${pkg.name}`);
}
for (const pkg of releases) {
  const response = await fetch(`https://registry.npmjs.org/${encodeURIComponent(pkg.name)}/${encodeURIComponent(pkg.version)}`, { signal: AbortSignal.timeout(30_000) });
  if (response.status === 200) { console.log(`Already published: ${pkg.name}@${pkg.version}`); continue; }
  if (response.status !== 404) throw new Error(`Registry lookup failed: ${pkg.name} HTTP ${response.status}`);
  execFileSync('npm', ['publish', resolve(dir, pkg.filename), '--access', 'public', '--registry', 'https://registry.npmjs.org', '--ignore-scripts', ...(live ? [] : ['--dry-run'])], { stdio: 'inherit' });
}
