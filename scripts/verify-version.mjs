import { readFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { resolve } from 'node:path';
import assert from 'node:assert/strict';
const read = async path => (await readFile(new URL(`../${path}`, import.meta.url), 'utf8')).replaceAll('\r\n', '\n');
const pkg = JSON.parse(await read('package.json')); const version = pkg.version;
const lock = JSON.parse(await read('package-lock.json'));
assert.equal(lock.version, version); assert.equal(lock.packages[''].version, version);
assert.equal(JSON.parse(await read('src-tauri/tauri.conf.json')).version, version);
for (const path of ['crates/pool-core/Cargo.toml','src-tauri/Cargo.toml']) {
  assert.equal((await read(path)).match(/^version = "([^"]+)"/m)?.[1], version, path);
}
const cargo = await read('Cargo.lock');
for (const name of ['pool-core','personal-codex-pool']) assert.ok(cargo.includes(`name = "${name}"\nversion = "${version}"`));
assert.ok((await read('README.md')).includes(`当前版本：${version}`));
assert.ok((await read('docs/ACCEPTANCE.md')).includes(`安装包、界面、EXE、Cargo/npm 版本均须为 ${version}`));
if (process.argv.includes('--artifacts')) {
  const dir = resolve('target/x86_64-pc-windows-msvc/release');
  const { stdout } = await promisify(execFile)('powershell.exe', ['-NoProfile','-Command', '$items = @($env:FUSION_VERSION_EXE, $env:FUSION_VERSION_INSTALLER) | ForEach-Object { (Get-Item -LiteralPath $_).VersionInfo | Select-Object ProductVersion,FileVersion }; ConvertTo-Json -Compress -InputObject $items'], {
    windowsHide:true, env:{...process.env, FUSION_VERSION_EXE:resolve(dir,'personal-codex-pool.exe'), FUSION_VERSION_INSTALLER:resolve(dir,`bundle/nsis/Fusion_${version}_x64-setup.exe`)}
  });
  for (const item of JSON.parse(stdout)) for (const key of ['ProductVersion','FileVersion']) assert.equal(item[key].split('.').slice(0,3).join('.'),version);
}
console.log(`Version ${version}: sources, documentation${process.argv.includes('--artifacts') ? ', EXE and installer' : ''} match; rendered UI is checked by startup.spec.ts.`);
