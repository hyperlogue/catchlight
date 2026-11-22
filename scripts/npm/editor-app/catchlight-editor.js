#!/usr/bin/env node
// Runs the `catchlight-editor` binary from the platform package npm installed
// beside this launcher, forwarding arguments, stdio and the exit status.
'use strict';
const { spawn } = require('node:child_process');

const pkg = `@catchlight/editor-${process.platform}-${process.arch}`;
const exe = process.platform === 'win32' ? 'catchlight-editor.exe' : 'catchlight-editor';
let binary;
try {
  binary = require.resolve(`${pkg}/bin/${exe}`);
} catch {
  console.error(
    `catchlight-editor: no build for ${process.platform} ${process.arch} is installed (${pkg}).\n` +
      'Reinstall without --omit=optional / --no-optional, or download a release from\n' +
      'https://github.com/hyperlogue/catchlight/releases',
  );
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit', windowsHide: false });
// The terminal delivers Ctrl-C to the binary too; wait for it to clean up
// (it removes its connection file) instead of exiting underneath it.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    if (signal !== 'SIGINT') child.kill(signal);
  });
}
child.on('error', (error) => {
  console.error(`catchlight-editor: could not start ${binary}: ${error.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
