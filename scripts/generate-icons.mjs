import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const output = join(root, 'target', 'icon-build');
const icons = join(root, 'src-tauri', 'icons');
const sizes = [16, 24, 32, 48, 64, 128, 256];

// Render the same flat SVG used by the UI with the project's existing Tauri CLI.
execFileSync(process.execPath, [
  join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'),
  'icon', join(root, 'public', 'fusion.svg'), '--output', output,
  ...sizes.flatMap(size => ['--png', String(size)]),
], { cwd: root, stdio: 'inherit' });

// ICO permits PNG frames; keep every Windows size without another image library.
const frames = sizes.map(size => readFileSync(join(output, `${size}x${size}.png`)));
const header = Buffer.alloc(6 + 16 * frames.length);
header.writeUInt16LE(1, 2);
header.writeUInt16LE(frames.length, 4);
let offset = header.length;
for (let i = 0; i < frames.length; i++) {
  const entry = 6 + 16 * i;
  header[entry] = header[entry + 1] = sizes[i] === 256 ? 0 : sizes[i];
  header.writeUInt16LE(1, entry + 4);
  header.writeUInt16LE(32, entry + 6);
  header.writeUInt32LE(frames[i].length, entry + 8);
  header.writeUInt32LE(offset, entry + 12);
  offset += frames[i].length;
}
mkdirSync(icons, { recursive: true });
writeFileSync(join(icons, 'icon.ico'), Buffer.concat([header, ...frames]));
for (const size of [32, 128, 256]) {
  writeFileSync(join(icons, `${size}x${size}.png`), frames[sizes.indexOf(size)]);
}
