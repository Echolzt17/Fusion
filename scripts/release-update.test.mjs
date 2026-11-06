import test from 'node:test';
import assert from 'node:assert/strict';
import { makeManifest, assetNames, checkAcceptance, checkPublisher, gates, validateNotes, publishStable, compareVersions, syncRepositoryOverview, repositoryDescription } from './release-update.mjs';

test('homepage synchronization changes only public README and About, and skips unchanged content', async () => {
  const content = '# Fusion\n\nPublic product description.\n', writes = [];
  let saved = { sha: 'current-readme', content: Buffer.from('old public readme').toString('base64') };
  let description = 'old description';
  const request = async (path, options) => {
    const file = path.endsWith('/contents/README.md');
    assert.ok(file || path === '/repos/Echolzt17/Fusion');
    if (!options.method) return file ? saved : { description };
    writes.push({ path, ...options });
    if (file) { assert.equal(options.body.sha, saved.sha); saved = { ...saved, content: options.body.content }; }
    else description = options.body.description;
  };
  await syncRepositoryOverview(content, 'public-fixture', request);
  assert.deepEqual(writes.map(w => w.method), ['PUT', 'PATCH']);
  assert.equal(Buffer.from(saved.content, 'base64').toString('utf8'), content);
  assert.deepEqual(writes[1].body, { description: repositoryDescription });
  writes.length = 0;
  await syncRepositoryOverview(content, 'public-fixture', request);
  assert.deepEqual(writes, []);
  await assert.rejects(syncRepositoryOverview('  ', 'public-fixture', request));
});
test('publication uses only the explicitly requested owner identity', () => {
  assert.doesNotThrow(() => checkPublisher({login:'Echolzt17'}));
  assert.doesNotThrow(() => checkPublisher({login:'echolzt17'}));
  for (const profile of [null, {}, {login:'other-publisher'}]) assert.throws(() => checkPublisher(profile));
});
test('release assets are an explicit binary-only allowlist with versioned URLs', () => {
  assert.deepEqual(assetNames('0.1.30'), ['Fusion_0.1.30_x64-setup.exe', 'Fusion_0.1.30_x64-setup.exe.sig', 'manifest.json']);
  const manifest = makeManifest('0.1.30', 'public-signature', '# Fusion 0.1.30\n\n## 本次重点\n- 更新入口更清晰。');
  assert.equal(manifest.platforms['windows-x86_64'].url, 'https://github.com/Echolzt17/Fusion/releases/download/v0.1.30/Fusion_0.1.30_x64-setup.exe');
  for (const version of ['../source', '0.1.30/other', 'v0.1.30', '']) assert.throws(() => makeManifest(version, 'sig', ''));
});
test('promotion fails closed until actual acceptance is recorded for this version', () => {
  assert.throws(() => checkAcceptance({ version: '0.1.30' }, '0.1.30'));
  const complete = { version: '0.1.30', ...Object.fromEntries(gates.map(gate => [gate, 'passed'])), evidence: 'Public test of the gate only; not actual acceptance.' };
  assert.doesNotThrow(() => checkAcceptance(complete, '0.1.30'));
  assert.throws(() => checkAcceptance({ ...complete, windows11: 'pending' }, '0.1.30'));
  assert.throws(() => checkAcceptance(complete, '0.1.31'));
  assert.throws(() => checkAcceptance({ ...complete, evidence: '' }, '0.1.30'));
});

test('only stable versions and useful version-matched release notes can be prepared', () => {
  const notes = '# Fusion 0.1.31\n\n## 本次重点\n- 更新按钮统一下载安装流程。';
  assert.doesNotThrow(() => validateNotes('0.1.31', notes));
  for (const invalid of ['', 'TODO', notes.replace('0.1.31', '0.1.30'), notes + '\n- 待补充', notes + '中'.repeat(16000)]) {
    assert.throws(() => validateNotes('0.1.31', invalid));
  }
  for (const version of ['0.1.31-beta.1', '0.1.31+build.1']) assert.throws(() => makeManifest(version, 'sig', notes));
});
test('stable publication verifies public bytes before updating compatibility then stable feeds', async () => {
  const calls = [], bytes = Buffer.from('signed public fixture');
  const dependencies = {
    api: async (path, args = {}) => {
      calls.push({path, ...args});
      if (path.includes('/tags/')) return null;
      if (args.method === 'POST' && !path.includes('/assets?')) return {id:1,draft:true,prerelease:false,assets:[]};
      return {};
    },
    publicDownload: async () => {calls.push({download:true}); return bytes;},
    updateFeed: async channel => calls.push({feed:channel}),
  };
  await publishStable({version:'0.1.31',manifest:{notes:'shared notes'},assets:[{name:'setup.exe',bytes}],token:'test-only'}, dependencies);
  const creation = calls.find(c => c.body?.draft === true);
  assert.equal(creation.body.prerelease, false);
  assert.deepEqual(calls.slice(-3), [{download:true},{feed:'preview'},{feed:'stable'}]);
  calls.length = 0;
  await assert.rejects(publishStable({version:'0.1.31',manifest:{notes:'shared notes'},assets:[{name:'setup.exe',bytes}],token:'test-only'},
    {...dependencies, publicDownload:async()=>Buffer.from('corrupt')}));
  assert.ok(!calls.some(c => c.feed));
});
test('published assets are never replaced and identical publication can resume feed updates', async () => {
  const bytes = Buffer.from('signed fixture'), calls=[];
  const input = {version:'0.1.31',manifest:{notes:'shared'},assets:[{name:'setup.exe',bytes}],token:'test-only'};
  const deps = {api:async () => ({id:1,draft:false,prerelease:false,body:'shared',assets:[{id:2,name:'setup.exe'}]}),
    publicDownload:async()=>bytes,updateFeed:async channel=>calls.push(channel)};
  await publishStable(input,deps); assert.deepEqual(calls,['preview','stable']);
  calls.length=0;
  await assert.rejects(publishStable(input,{...deps,publicDownload:async()=>Buffer.from('other')}));
  assert.deepEqual(calls,[]);
});

test('explicit online test labels prereleases and repair upload never advances discovery feeds', async () => {
  const calls=[], bytes=Buffer.from('signed test fixture');
  const dependencies={
    api:async(path,args={})=>{
      calls.push({path,...args});
      if(path.includes('/tags/'))return null;
      if(args.method==='POST'&&!path.includes('/assets?'))return {id:1,draft:true,assets:[]};
      return {};
    },
    publicDownload:async()=>bytes,
    updateFeed:async channel=>calls.push({feed:channel}),
  };
  const input={version:'0.1.32',manifest:{notes:'shared'},assets:[{name:'setup.exe',bytes}],token:'test-only',testOnly:true};
  await publishStable({...input,publishFeeds:false},dependencies);
  assert.ok(!calls.some(c=>c.feed));
  const published=calls.find(c=>c.method==='PATCH');
  assert.equal(published.body.prerelease,true);assert.equal(published.body.make_latest,'false');
  assert.match(published.body.body,/仍待验收/);
  calls.length=0;
  await publishStable(input,dependencies);
  assert.deepEqual(calls.slice(-2),[{feed:'preview'},{feed:'stable'}]);
  calls.length=0;
  await assert.rejects(publishStable(input,{...dependencies,publicDownload:async()=>Buffer.from('corrupt')}));
  assert.ok(!calls.some(c=>c.feed));
});

test('offline server exposes only verified versioned assets and target metadata on loopback', async t => {
  const {mkdtemp,mkdir,writeFile,rm}=await import('node:fs/promises');
  const {tmpdir}=await import('node:os'); const {join}=await import('node:path');
  const {createAcceptanceServer}=await import('./serve-update-acceptance.mjs');
  const root=await mkdtemp(join(tmpdir(),'fusion-update-server-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const folders=[];
  for(const version of ['0.1.31','0.1.30']){
    const folder=join(root,version);await mkdir(folder);folders.push(folder);
    const manifest=makeManifest(version,'public-sig',`# Fusion ${version}\n## 本次重点\n- 公开测试材料，不是真实升级验收。`);
    await writeFile(join(folder,'manifest.json'),JSON.stringify(manifest));
    await writeFile(join(folder,assetNames(version)[0]),'public fixture '+version);
    await writeFile(join(folder,assetNames(version)[1]),'public-sig');
  }
  await assert.rejects(createAcceptanceServer(folders,async()=>{throw new Error('invalid signature');}));
  const verified=[];const server=await createAcceptanceServer(folders,async(_,v)=>verified.push(v));
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(async()=>{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));});
  const origin=`http://127.0.0.1:${server.address().port}`;
  assert.deepEqual(verified,['0.1.31','0.1.30']);
  assert.equal((await (await fetch(origin+'/stable.json')).json()).version,'0.1.31');
  assert.equal(await (await fetch(origin+'/v0.1.30/Fusion_0.1.30_x64-setup.exe')).text(),'public fixture 0.1.30');
  for(const route of ['/','/package.json','/v0.1.31/../manifest.json'])assert.equal((await fetch(origin+route)).status,404);
  assert.equal((await fetch(origin+'/stable.json',{method:'POST'})).status,405);
});

test('stable release ordering cannot confuse 0.1.9 with 0.1.31 or silently accept preview labels',()=>{
  assert.equal(compareVersions('0.1.31','0.1.9'),1);
  assert.equal(compareVersions('0.1.30','0.1.31'),-1);
  assert.equal(compareVersions('0.1.31','0.1.31'),0);
  assert.throws(()=>compareVersions('0.1.31-beta.1','0.1.31'));
});
