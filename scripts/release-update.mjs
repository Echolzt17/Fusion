import { readFile, writeFile, mkdir, copyFile, stat, mkdtemp, unlink, rmdir } from 'node:fs/promises';
import { spawnSync, execFile } from 'node:child_process';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { promisify } from 'node:util';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';

export const repository = 'Echolzt17/Fusion';
export const repositoryDescription = 'Windows 10/11 x64 Codex 本机账号池｜额度调度、同聊天可读历史续接、本机项目入口、用量可视化、一键启停及签名更新。含开发源码。';
const project = fileURLToPath(new URL('../', import.meta.url));
const releaseRoot = resolve(project, 'target/x86_64-pc-windows-msvc/release');
export const gates = ['windows10', 'windows11', 'realUpdate', 'accountsAndBindings', 'activeRequestDrain', 'independentRepair', 'fourConversations', 'teamNetwork'];
export function checkPublisher(profile) {
  if (typeof profile?.login !== 'string' || profile.login.toLowerCase() !== 'echolzt17') throw new Error('Publishing identity is not Echolzt17. Nothing was uploaded; use the repository owner account.');
}
export function assetNames(version) { return [`Fusion_${version}_x64-setup.exe`, `Fusion_${version}_x64-setup.exe.sig`, 'manifest.json']; }
function versionCheck(version) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) throw new Error('Invalid release version.');
}
export function compareVersions(left, right) {
  versionCheck(left); versionCheck(right);
  const a=left.split('.').map(BigInt), b=right.split('.').map(BigInt);
  for(let i=0;i<3;i++) if(a[i]!==b[i]) return a[i]>b[i]?1:-1;
  return 0;
}
async function stableVersion(token) {
  const file = await api(`/repos/${repository}/contents/updates/stable.json`, {token});
  if (!file) return null;
  const value = JSON.parse(Buffer.from(file.content, 'base64').toString('utf8'));
  versionCheck(value.version); return value.version;
}
export function validateNotes(version, notes) {
  if (typeof notes !== 'string' || Buffer.byteLength(notes, 'utf8') > 16000
    || notes.split(/\r?\n/)[0] !== `# Fusion ${version}`
    || !/^## 本次重点\s*$/m.test(notes) || !/^[-*] \S.+/m.test(notes)
    || /\b(?:TODO|TBD|placeholder)\b|待补充|请填写/i.test(notes)) {
    throw new Error('Write version-matched, meaningful release notes with 本次重点 before preparing a release.');
  }
}
export function makeManifest(version, signature, notes) {
  versionCheck(version);
  validateNotes(version, notes);
  if (!signature.trim()) throw new Error('Missing signature or excessive release notes.');
  return { version, notes, pub_date: new Date().toISOString(), platforms: { 'windows-x86_64': {
    url: `https://github.com/${repository}/releases/download/v${version}/${assetNames(version)[0]}`, signature: signature.trim(),
  } } };
}
export function checkAcceptance(value, version) {
  if (value.version !== version || gates.some(gate => value[gate] !== 'passed') || !value.evidence?.trim()) {
    throw new Error('Real acceptance is incomplete. Record actual Win10/Win11, update, recovery, account/binding, drain, concurrency and team-network results before publication.');
  }
}
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, { cwd: project, stdio: 'inherit', windowsHide: true, env });
  if (result.error || result.status !== 0) throw new Error(`${command} failed; release stopped.`);
}
const npm = script => run(process.execPath, [resolve(process.env.APPDATA || '', 'npm/node_modules/npm/bin/npm-cli.js'), 'run', script]);
// Use the npm entrypoint supplied by npm run, avoiding command-shell interpolation.
function npmRun(script) {
  if (process.env.npm_execpath) run(process.execPath, [process.env.npm_execpath, 'run', script]);
  else if (process.platform === 'win32') run('powershell.exe', ['-NoProfile', '-Command', `& npm.cmd run ${script}; exit $LASTEXITCODE`]);
  else npm(script);
}
async function api(path, { method = 'GET', body, binary, binaryRead, token } = {}) {
  if (!token) throw new Error('Set FUSION_GITHUB_TOKEN for a maintainer with write access to Echolzt17/Fusion. No token is bundled in the app.');
  if (!/^(github_pat_|gho_|ghu_|ghp_)[A-Za-z0-9_]+$/.test(token)) throw new Error('Use a valid existing GitHub authorization or enter a valid token in the local window.');
  const base = binary ? 'https://uploads.github.com' : 'https://api.github.com';
  const response = await fetch(base + path, { method, headers: {
    Authorization: `Bearer ${token}`, Accept: binaryRead ? 'application/octet-stream' : 'application/vnd.github+json',
    'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'Fusion-release',
    ...(body ? { 'Content-Type': binary ? 'application/octet-stream' : 'application/json' } : {}),
  }, body: body ? binary ? body : JSON.stringify(body) : undefined, signal: AbortSignal.timeout(binary ? 1800000 : 60000) });
  if (response.status === 404 && method === 'GET') return null;
  if (!response.ok) throw new Error(`GitHub ${method} failed (${response.status}); no source code was uploaded. Check repository permissions and retry.`);
  return response.status === 204 ? null : binaryRead ? Buffer.from(await response.arrayBuffer()) : response.json();
}
async function updateFeed(channel, manifest, token) {
  if (!['stable', 'preview'].includes(channel)) throw new Error('Invalid channel.');
  const path = `/repos/${repository}/contents/updates/${channel}.json`;
  const previous = await api(path, { token });
  await api(path, { method: 'PUT', token, body: { message: `${channel}: v${manifest.version}`, content: Buffer.from(JSON.stringify(manifest, null, 2) + '\n').toString('base64'), ...(previous?.sha ? { sha: previous.sha } : {}) } });
}
export async function syncRepositoryOverview(content, token, request = api) {
  if (!content.trim()) throw new Error('Public README is empty. Nothing was uploaded.');
  const path = `/repos/${repository}/contents/README.md`;
  const existing = await request(path, { token });
  if (!existing || Buffer.from(existing.content || '', 'base64').toString('utf8') !== content) {
    await request(path, { method: 'PUT', token, body: { message: 'Document Fusion purpose, technology and stability',
      ...(existing?.sha ? { sha: existing.sha } : {}), content: Buffer.from(content).toString('base64') } });
  }
  const repo = await request(`/repos/${repository}`, { token });
  if (repo.description !== repositoryDescription) {
    await request(`/repos/${repository}`, { method: 'PATCH', token, body: { description: repositoryDescription } });
  }
}
async function publicDownload(url) {
  // Intentionally no GitHub authorization header: test exactly what clients can fetch.
  // Node fetch does not use the current Windows system proxy. Use the native
  // system-network path for public assets, still comparing every byte below.
  if (process.platform === 'win32') {
    const directory = await mkdtemp(join(tmpdir(), 'fusion-public-update-'));
    const path = join(directory, 'asset');
    try {
      await promisify(execFile)('powershell.exe', ['-NoProfile','-ExecutionPolicy','Bypass','-File',
        join(project,'scripts/download-public-update.ps1'),'-Url',url,'-OutFile',path], {
        windowsHide:true, timeout:1810000, env:{...process.env,FUSION_GITHUB_TOKEN:''},
      });
      return await readFile(path);
    } catch { throw new Error('Public release download through the Windows system network failed; feeds were not changed.'); }
    finally { await unlink(path).catch(()=>{}); await rmdir(directory).catch(()=>{}); }
  }
  const response = await fetch(url, { signal: AbortSignal.timeout(1800000) });
  if (!response.ok) throw new Error(`Public release download failed (${response.status}); feed was not changed.`);
  return Buffer.from(await response.arrayBuffer());
}
async function verifyFolder(folder, version) {
  const names = assetNames(version); const manifest = JSON.parse(await readFile(join(folder, 'manifest.json'), 'utf8'));
  const signature = (await readFile(join(folder, names[1]), 'utf8')).trim();
  const entry = manifest.platforms?.['windows-x86_64'];
  if (entry?.signature !== signature || entry?.url !== `https://github.com/${repository}/releases/download/v${version}/${names[0]}` || manifest.version !== version) throw new Error('Prepared release does not match its fixed version and signature.');
  run(join(releaseRoot, 'personal-codex-pool.exe'), ['--verify-update-package', join(folder, 'manifest.json'), join(folder, names[0])]);
  return manifest;
}
async function publishedRelease(version, token) {
  const release = await api(`/repos/${repository}/releases/tags/v${version}`, { token });
  if (!release || release.draft) throw new Error('The public release is not available.');
  for (const name of assetNames(version)) if (!release.assets.some(a => a.name === name)) throw new Error(`Missing release asset: ${name}`);
  return release;
}
export async function publishStable({ version, manifest, assets, token, testOnly = false, publishFeeds = true }, dependencies = {}) {
  const request = dependencies.api || api;
  const download = dependencies.publicDownload || publicDownload;
  const feed = dependencies.updateFeed || updateFeed;
  const notes = testOnly
    ? '本版本用于维护者明确要求的在线更新测试。Windows 10/11、账号保留、独立修复及长时间使用仍待验收，不代表团队稳定上线。\n\n' + manifest.notes
    : manifest.notes;
  if (!dependencies.api) console.log(`Checking release v${version}.`);
  let release = await request(`/repos/${repository}/releases/tags/v${version}`, { token });
  if (!release) release = await request(`/repos/${repository}/releases`, { method: 'POST', token,
    body: { tag_name: `v${version}`, name: `Fusion ${version}${testOnly ? ' · 在线更新测试' : ''}`, body: notes, draft: true, prerelease: testOnly } });
  if (!release.draft && (!!release.prerelease !== testOnly || release.body !== notes)) throw new Error('Published release differs. Use a new version; existing assets were preserved.');
  const publiclyVerified = new Set();
  for (const { name, bytes } of assets) {
    const existing = release.assets.find(a => a.name === name);
    if (!dependencies.api) console.log(`${existing ? 'Verifying existing' : 'Uploading'} v${version}/${name} (${bytes.length} bytes).`);
    if (existing) {
      const actual = release.draft
        ? await request(`/repos/${repository}/releases/assets/${existing.id}`, { token, binaryRead: true })
        : await download(`https://github.com/${repository}/releases/download/v${version}/${name}`);
      if (!Buffer.isBuffer(actual) || !actual.equals(bytes)) throw new Error('Existing asset differs. Use a new version; nothing was replaced.');
      if (!release.draft) publiclyVerified.add(name);
    } else {
      if (!release.draft) throw new Error('Published release is incomplete. Use a new version.');
      await request(`/repos/${repository}/releases/${release.id}/assets?name=${encodeURIComponent(name)}`, { method: 'POST', token, body: bytes, binary: true });
    }
  }
  if (release.draft) await request(`/repos/${repository}/releases/${release.id}`, { method: 'PATCH', token,
    body: { body: notes, draft: false, prerelease: testOnly, make_latest: testOnly ? 'false' : 'true' } });
  for (const { name, bytes } of assets) {
    if (publiclyVerified.has(name)) continue;
    if (!dependencies.api) console.log(`Verifying anonymous download v${version}/${name}.`);
    const actual = await download(`https://github.com/${repository}/releases/download/v${version}/${name}`);
    if (!actual.equals(bytes)) throw new Error('Public release bytes differ; feeds were not changed.');
  }
  if (publishFeeds) {
    await feed('preview', manifest, token); // Old client compatibility URL.
    await feed('stable', manifest, token); // Explicit online tests use this same installed-client endpoint.
  }
}
async function main() {
  const [action, ...args] = process.argv.slice(2);
  const option = name => { const i = args.indexOf(`--${name}`); return i >= 0 ? args[i + 1] : undefined; };
  const pkg = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
  const version = option('version') || pkg.version; versionCheck(version);
  const folder = join(releaseRoot, 'publish', `v${version}`);
  const token = process.env.FUSION_GITHUB_TOKEN;
  if (['authorize', 'status', 'initialize', 'publish', 'publish-test', 'promote', 'withdraw'].includes(action)) {
    checkPublisher(await api('/user', {token}));
    console.log('Publishing identity verified: Echolzt17.');
    if (action === 'authorize') return;
  }
  if (action === 'status') {
    const release = await api(`/repos/${repository}/releases/tags/v${version}`, {token});
    console.log(JSON.stringify({version,release:release ? {id:release.id,draft:release.draft,prerelease:release.prerelease,
      assets:release.assets.map(({id,name,size,state})=>({id,name,size,state}))} : null}));
    return;
  }
  if (action === 'prepare') {
    if (version !== pkg.version) throw new Error('Prepare must use the current project version.');
    const notes = await readFile(resolve(project, option('notes') || 'docs/UPDATE_NOTES.md'), 'utf8');
    for (const script of ['check', 'test', 'test:updates', 'test:ui']) npmRun(script);
    run('cargo', ['test', '-p', 'personal-codex-pool']);
    npmRun('build'); npmRun('test:installer');
    run(process.execPath, ['scripts/verify-version.mjs', '--artifacts']);
    const names = assetNames(version); await mkdir(folder, { recursive: true });
    for (const name of names.slice(0, 2)) await copyFile(join(releaseRoot, 'bundle/nsis', name), join(folder, name));
    const manifest = makeManifest(version, await readFile(join(folder, names[1]), 'utf8'), notes);
    await writeFile(join(folder, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
    try { await stat(join(folder, 'acceptance.json')); } catch {
      await writeFile(join(folder, 'acceptance.json'), JSON.stringify({ version, ...Object.fromEntries(gates.map(gate => [gate, 'pending'])), githubOnlineUpdate: 'pending', evidence: '' }, null, 2) + '\n');
    }
    await verifyFolder(folder, version);
    console.log(`Prepared signed release ${version}. Actual acceptance remains pending; nothing was uploaded.`); return;
  }
  if (action === 'initialize') {
    await syncRepositoryOverview(await readFile(join(project, 'docs/PUBLIC_README.md'), 'utf8'), token);
    console.log('Synchronized public README and repository description; no application source or feeds were uploaded.'); return;
  }
  if (action === 'publish' || action === 'publish-test') {
    const testOnly = action === 'publish-test';
    if (!testOnly) checkAcceptance(JSON.parse(await readFile(resolve(option('acceptance') || join(folder, 'acceptance.json')), 'utf8')), version);
    const current = await stableVersion(token);
    if (current && compareVersions(version, current) < 0) throw new Error('An older version cannot replace the stable feed. Use withdraw explicitly for a rollback.');
    const manifest = await verifyFolder(folder, version);
    validateNotes(version, manifest.notes);
    // Only the explicitly invoked online test may publish while acceptance stays
    // pending. First provide the installed version's signed repair material.
    if (testOnly) {
      const repairVersion = option('repair-version');
      if (!repairVersion || compareVersions(repairVersion, version) >= 0) throw new Error('Online update test requires the installed older --repair-version.');
      const repairFolder = join(releaseRoot, 'publish', `v${repairVersion}`);
      const repairManifest = await verifyFolder(repairFolder, repairVersion);
      validateNotes(repairVersion, repairManifest.notes);
      const repairAssets = await Promise.all(assetNames(repairVersion).map(async name => ({name, bytes:await readFile(join(repairFolder,name))})));
      await publishStable({version:repairVersion,manifest:repairManifest,assets:repairAssets,token,testOnly:true,publishFeeds:false});
      console.log(`Installed-version repair assets v${repairVersion} published and publicly verified; feeds unchanged.`);
    }
    const assets = await Promise.all(assetNames(version).map(async name => ({ name, bytes: await readFile(join(folder, name)) })));
    await publishStable({ version, manifest, assets, token, testOnly });
    console.log(testOnly
      ? `Online update test ${version} published; both discovery feeds match. All actual acceptance files remain unchanged.`
      : `Stable ${version} published; stable feed and old-client compatibility feed match. Online upgrade acceptance remains separate.`);
    return;
  }
  if (action === 'promote' || action === 'withdraw') {
    if (action === 'promote') checkAcceptance(JSON.parse(await readFile(resolve(option('acceptance') || join(folder, 'acceptance.json')), 'utf8')), version);
    const release = await publishedRelease(version, token);
    const manifestBytes = await publicDownload(`https://github.com/${repository}/releases/download/v${version}/manifest.json`);
    const manifest = JSON.parse(manifestBytes);
    const names = assetNames(version); const repair = join(releaseRoot, 'publish-verify', `v${version}`); await mkdir(repair, { recursive: true });
    await writeFile(join(repair, 'manifest.json'), manifestBytes);
    for (const name of names.slice(0, 2)) await writeFile(join(repair, name), await publicDownload(`https://github.com/${repository}/releases/download/v${version}/${name}`));
    await verifyFolder(repair, version);
    const current = action === 'promote' ? await stableVersion(token) : null;
    const historical = !!current && compareVersions(version, current) < 0;
    if (action === 'promote') await api(`/repos/${repository}/releases/${release.id}`, { method: 'PATCH', token, body: { prerelease: false, make_latest: historical ? 'false' : 'true', body: '该版本已完成线下真实验收，原安装包、签名及下载地址保持不变。\n\n历史发布说明：\n\n' + manifest.notes } });
    else if (release.prerelease) throw new Error('Withdrawal target must be an existing stable release.');
    if (historical) { console.log(`Historical ${version} is now formal; newer stable ${current} and both feeds were preserved.`); return; }
    await updateFeed('preview', manifest, token); // Compatibility alias for installed 0.1.30 clients.
    await updateFeed('stable', manifest, token);
    console.log(`Stable feed now points to ${version}. Installed newer versions are never downgraded automatically.`); return;
  }
  throw new Error('Usage: npm run updates:release -- prepare | initialize | publish --acceptance FILE | publish-test --repair-version INSTALLED_VERSION | promote --version LEGACY_VERSION --acceptance FILE | withdraw --version LAST_GOOD_VERSION');
}
if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) main().catch(error => { console.error(error.message); process.exitCode = 1; });
