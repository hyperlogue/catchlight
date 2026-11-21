// Build standalone npm artifacts without changing workspace source exports.
// Only this explicit set is released; WASM stays host-supplied and private.
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { execFileSync } from 'node:child_process';

const root = resolve(import.meta.dirname, '../..');
process.chdir(root);
const output = resolve('dist/npm');
rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
const paths = {};
const releases = [];
for (const name of ['core', 'react', 'editor']) {
  const source = resolve('packages', name);
  const dest = resolve(output, name);
  mkdirSync(dest, { recursive: true });
  const config = resolve(output, `tsconfig-${name}.json`);
  writeFileSync(config, JSON.stringify({
    extends: resolve(source, 'tsconfig.json'),
    compilerOptions: { noEmit: false, declaration: true, types: [],
      lib: ['ES2022', 'DOM', 'DOM.Iterable', 'ESNext.Disposable'],
      rootDir: resolve(source, 'src'), outDir: resolve(dest, 'dist'), paths },
    include: [resolve(source, 'src/**/*.ts'), resolve(source, 'src/**/*.tsx')],
    exclude: [resolve(source, 'src/**/*.test.*'), resolve(source, 'src/test/**')],
  }));
  execFileSync(process.execPath, ['node_modules/typescript/bin/tsc', '-p', config], { stdio: 'inherit' });
  rmSync(config);
  const pkg = JSON.parse(readFileSync(resolve(source, 'package.json'), 'utf8'));
  if (pkg.name !== `@catchlight/${name}` || pkg.private || releases.some(p => p.version !== pkg.version)) {
    throw new Error('Expected three public @catchlight packages with matching versions');
  }
  const exports = {};
  for (const [key, value] of Object.entries(pkg.exports)) {
    const target = value.replace('./src/', './dist/');
    exports[key] = target.endsWith('.ts')
      ? { types: target.replace(/\.ts$/, '.d.ts'), import: target.replace(/\.ts$/, '.js'), default: target.replace(/\.ts$/, '.js') }
      : target;
  }
  pkg.main = './dist/index.js';
  pkg.types = './dist/index.d.ts';
  pkg.exports = exports;
  pkg.files = ['dist', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md'];
  pkg.sideEffects = name === 'editor' ? ['**/*.css'] : false;
  for (const [peer, range] of Object.entries(pkg.peerDependencies ?? {})) {
    if (range === 'workspace:*') {
      const dependency = releases.find(p => p.name === peer);
      if (!dependency) throw new Error(`Missing release dependency: ${peer}`);
      pkg.peerDependencies[peer] = `^${dependency.version}`;
    }
  }
  delete pkg.devDependencies;
  delete pkg.scripts;
  writeFileSync(resolve(dest, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');
  cpSync(resolve(source, 'README.md'), resolve(dest, 'README.md'));
  for (const file of ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md']) {
    cpSync(file, resolve(dest, file));
  }
  if (name === 'editor') {
    cpSync(resolve(source, 'src/theme.css'), resolve(dest, 'dist/theme.css'));
    cpSync('assets/logo-transparent.svg', resolve(dest, 'dist/logo.svg'));
  }
  paths[pkg.name] = [resolve(dest, 'dist/index.d.ts')];
  if (name === 'core') paths['@catchlight/core/fakes'] = [resolve(dest, 'dist/fakes.d.ts')];
  const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', output], { cwd: dest, encoding: 'utf8' }))[0];
  releases.push({ name: pkg.name, version: pkg.version, filename: packed.filename, integrity: packed.integrity });
}
writeFileSync(resolve(output, 'releases.json'), JSON.stringify(releases, null, 2) + '\n');
console.log('Packed:', releases.map(p => p.filename).join(', '));
