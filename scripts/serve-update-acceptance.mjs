// Local-only real installer acceptance. No credentials, upload, installation or process control.
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { resolve, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { assetNames } from './release-update.mjs';

export async function createAcceptanceServer(folders, verify) {
  if (folders.length < 2) throw new Error('Provide the target release folder first, then the installed-version repair folder.');
  const files = new Map(); let target;
  for (const folder of folders) {
    const manifest = JSON.parse(await readFile(join(folder, 'manifest.json'), 'utf8'));
    const version = manifest.version;
    if (!/^\d+\.\d+\.\d+$/.test(version) || files.has(`/v${version}/manifest.json`)) throw new Error('Invalid or repeated version.');
    const [installer, signature] = assetNames(version);
    if (manifest.platforms?.['windows-x86_64']?.url !== `https://github.com/Echolzt17/Fusion/releases/download/v${version}/${installer}`) throw new Error('Unexpected update URL.');
    if (manifest.platforms['windows-x86_64'].signature !== (await readFile(join(folder, signature), 'utf8')).trim()) throw new Error('Manifest signature differs from signature asset.');
    await verify(folder, version);
    target ||= manifest;
    for (const name of assetNames(version)) {
      const path = resolve(folder, name), size = (await stat(path)).size;
      files.set(`/v${version}/${name}`, { path, size });
    }
  }
  const feed = Buffer.from(JSON.stringify(target));
  return createServer((request, response) => {
    if (request.method !== 'GET') { response.writeHead(405).end(); return; }
    if (request.url === '/stable.json') {
      response.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': feed.length, 'Cache-Control': 'no-store' });
      response.end(feed); return;
    }
    const file = files.get(request.url);
    if (!file) { response.writeHead(404).end(); return; }
    response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': file.size, 'Cache-Control': 'no-store' });
    const stream = createReadStream(file.path);
    stream.on('error', () => response.destroy());
    response.on('close', () => stream.destroy());
    stream.pipe(response);
  });
}
if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    const verifier = resolve('target/x86_64-pc-windows-msvc/release/personal-codex-pool.exe');
    const server = await createAcceptanceServer(process.argv.slice(2).map(p => resolve(p)), async (folder, version) => {
      const result = spawnSync(verifier, ['--verify-update-package', join(folder, 'manifest.json'), join(folder, assetNames(version)[0])], { windowsHide: true, stdio: 'ignore' });
      if (result.error || result.status !== 0) throw new Error('Package signature verification failed; local server was not started.');
    });
    server.listen(0, '127.0.0.1', () => console.log(`Local acceptance origin: http://127.0.0.1:${server.address().port}\nOnly update-acceptance builds can use this origin. No installation was started.`));
  } catch (e) { console.error(e.message); process.exitCode = 1; }
}
