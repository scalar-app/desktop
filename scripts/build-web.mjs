/**
 * Builds the Scalar web app and copies it in as this app's frontend.
 *
 * The desktop and mobile apps are not a second interface. They are the same web app, built as
 * static files and loaded from inside the bundle, so a change to a screen reaches every platform
 * without being written twice.
 *
 * `web` is a sibling checkout, the same arrangement the `link:` dependencies use elsewhere.
 */
import { spawn } from 'node:child_process';
import { cp, mkdir, rm, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const desktopDir = resolve(here, '..');
const webDir = process.env.SCALAR_WEB_DIR
  ? resolve(process.env.SCALAR_WEB_DIR)
  : resolve(desktopDir, '..', 'web');
const exportDir = join(webDir, 'out');
const destination = join(desktopDir, 'dist');

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

function run(command, args, cwd) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: 'inherit',
      shell: process.platform === 'win32',
    });
    child.on('error', reject);
    child.on('exit', (code) =>
      code === 0 ? resolvePromise() : reject(new Error(`${command} exited with ${String(code)}`)),
    );
  });
}

if (!(await exists(webDir))) {
  console.error(
    `Could not find the web app at ${webDir}.\n` +
      'Clone scalar-app/web next to this repository, or set SCALAR_WEB_DIR.',
  );
  process.exit(1);
}

// No NEXT_PUBLIC_API_URL on purpose. A packaged app has no default server to point at, so it asks
// on first run and remembers the answer.
await run('pnpm', ['install'], webDir);
await run('pnpm', ['build:static'], webDir);

if (!(await exists(exportDir))) {
  console.error(`The web build produced no ${exportDir}. Check the output above.`);
  process.exit(1);
}

await rm(destination, { recursive: true, force: true });
await mkdir(destination, { recursive: true });
await cp(exportDir, destination, { recursive: true });

console.log(`Copied the web app into ${destination}`);
