import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('../deploy/proxy/fusion-fallback.js', import.meta.url), 'utf8');
for (const names of [[], ['primary'], ['backup'], ['primary', 'backup']]) {
  const context = {};
  runInNewContext(source.replace('__FUSION_PRIMARY__', 'primary').replace('__FUSION_BACKUP__', 'backup'), context);
  const input = { proxies: names.map(name => ({ name, type: 'reject' })),
    'proxy-groups': [{ name: 'original', type: 'select', proxies: ['REJECT'] }], rules: ['MATCH,original'] };
  const before = JSON.stringify(input.proxies);
  const output = context.main(input);
  const group = output['proxy-groups'][0];
  assert.equal(JSON.stringify(group.proxies), JSON.stringify(names.length ? names : ['REJECT']));
  assert.equal(group.type, 'fallback'); assert.equal(group.interval, 30); assert.equal(group.timeout, 5000);
  assert.equal(group['empty-fallback'], 'REJECT'); assert.equal(group.lazy, false);
  assert.equal(JSON.stringify(output.proxies), before);
  assert.equal(output.rules.at(-1), 'MATCH,original'); assert.equal(output.rules.length, 3);
  assert.ok(output.rules.slice(0,2).every(r=>r.includes('(PROCESS-NAME,personal-codex-pool.exe)')));
  assert.throws(()=>context.main(output));
}
console.log('Fallback: ordered existing nodes, empty REJECT, process+domain scope, unrelated configuration preserved.');
