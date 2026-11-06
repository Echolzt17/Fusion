// Isolated protocol acceptance against the actual installed Codex engine.
// Never reads the user's auth/config or calls a real inference service.
import { createServer } from 'node:http';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, writeFile, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';

const engine = process.argv[2];
const rejectImage = process.argv.includes('--failure');
const gatewayArg = process.argv.indexOf('--gateway');
const externalGateway = gatewayArg >= 0 ? process.argv[gatewayArg + 1] : null;
if (!engine) throw new Error('Usage: node scripts/verify-native-images.mjs <codex.exe>');
let version;
const root = await mkdtemp(join(tmpdir(), 'fusion-native-images-'));
const key = 'fusion-public-fixture-key';
const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';
const requests = [];
let calls = 0;
let failure;
const server = createServer(async (req, res) => {
  try {
    if (!req.url.startsWith('/v1/')) { req.resume(); res.setHeader('content-type','application/json'); res.end(JSON.stringify({plan_type:'plus',rate_limit:{allowed:true,limit_reached:false}})); return; }
    const chunks = [];
    for await (const b of req) chunks.push(b);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : {};
    assert.ok(req.headers['x-fusion-client-key'] === key, 'local fixture header');
    delete req.headers.authorization;
    assert.equal(req.headers['x-openai-actor-authorization'], 'fusion-local');
    requests.push(req.url.split('?')[0]);
    if (req.url.startsWith('/v1/models')) {
      res.setHeader('content-type', 'application/json');
      return res.end(JSON.stringify({ models: [], data: [] }));
    }
    if (req.url === '/v1/images/generations' || req.url === '/v1/images/edits') {
      if (req.url.endsWith('/edits')) assert.equal(body.images.length, 1);
      assert.ok(req.headers['x-codex-image-turn-id']);
      res.setHeader('content-type', 'application/json');
      res.setHeader('x-codex-imagegen-request-id', 'fixture-image-request');
      if (rejectImage) { res.writeHead(500); return res.end(JSON.stringify({ error: { message: 'fixture failure', type: 'server_error' } })); }
      return res.end(JSON.stringify({ created: 1, background: 'opaque', data: [{ b64_json: png, generation_id: 'fixture-generation' }] }));
    }
    assert.equal(req.url, '/v1/responses');
    // Responses Lite carries declarations as additional_tools input items.
    const toolText = JSON.stringify([body.tools, ...(body.input ?? []).filter(i => i.type === 'additional_tools')]);
    assert.ok(toolText.includes('image_gen') && toolText.includes('imagegen'), 'native imagegen must be advertised');
    const n = calls++;
    const item = n < (rejectImage ? 1 : 2)
      ? { type: 'function_call', id: `fc-${n}`, call_id: `image-${n}`, namespace: 'image_gen', name: 'imagegen', arguments: JSON.stringify({ prompt: 'Fixture only', ...(n === 1 ? { num_last_images_to_include: 1 } : {}) }) }
      : { type: 'message', id: 'msg-fixture', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'fixture-ok', annotations: [] }] };
    const events = [
      { type: 'response.created', response: { id: `resp-${n}`, object: 'response', status: 'in_progress' } },
      { type: 'response.output_item.added', output_index: 0, item },
      { type: 'response.output_item.done', output_index: 0, item },
      { type: 'response.completed', response: { id: `resp-${n}`, object: 'response', status: 'completed', output: [item], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } },
    ];
    res.setHeader('content-type', 'text/event-stream');
    res.end(events.map(e => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''));
  } catch (e) { failure ??= e; res.writeHead(500).end(); }
});
try {
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  await writeFile(join(root, 'config.toml'), `model = "gpt-6.1-sol"
model_provider = "fusion_fixture"
cli_auth_credentials_store = "file"
chatgpt_base_url = "http://127.0.0.1:${server.address().port}"
[features]
image_generation = true
[analytics]
enabled = false
[model_providers.fusion_fixture]
name = "Fusion Fixture"
base_url = "${externalGateway || `http://127.0.0.1:${server.address().port}/v1` }"
wire_api = "responses"
requires_openai_auth = true
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
http_headers = { "x-openai-actor-authorization" = "fusion-local", "x-fusion-client-key" = "fusion-public-fixture-key" }
`);
  // Public fake ChatGPT login: the real login and credential store are isolated.
  const claims = {exp:4102444800,sub:'fixture-user',email:'fixture@example.invalid',
    'https://api.openai.com/auth':{chatgpt_plan_type:'plus',chatgpt_account_id:'fixture-account',chatgpt_user_id:'fixture-user',user_id:'fixture-user'}};
  const jwt = Buffer.from('{"alg":"none"}').toString('base64url')+'.'+Buffer.from(JSON.stringify(claims)).toString('base64url')+'.c2ln';
  await writeFile(join(root,'auth.json'), JSON.stringify({auth_mode:'chatgpt',OPENAI_API_KEY:null,
    tokens:{id_token:jwt,access_token:jwt,refresh_token:'public-fixture',account_id:'fixture-account'},last_refresh:new Date().toISOString()}));
  const env = {};
  for (const k of ['SYSTEMROOT','WINDIR','SYSTEMDRIVE','COMSPEC','PATH']) if(process.env[k]) env[k]=process.env[k];
  for (const k of ['CODEX_HOME','USERPROFILE','HOME','LOCALAPPDATA','APPDATA','TEMP','TMP']) env[k]=root;
  env.OTEL_SDK_DISABLED='true';
  for (const k of Object.keys(env)) if (/OPENAI|CODEX.*TOKEN|CHATGPT/i.test(k)) delete env[k];
  env.CODEX_HOME = root;
  version = execFileSync(engine, ['--version'], { encoding:'utf8', windowsHide:true, env, cwd:root }).trim();
  const child = spawn(resolve(engine), ['exec', '--skip-git-repo-check', '--ephemeral', '--ignore-rules', '--color', 'never', '-C', root, 'Fixture'], { env, cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', b => { output += b; });
  child.stderr.on('data', b => { output += b; });
  const timer = setTimeout(() => child.kill(), 90_000);
  const code = await new Promise((r, reject) => { child.on('error', reject); child.on('exit', r); });
  clearTimeout(timer);
  if (failure) throw failure;
  assert.equal(code, 0, output.slice(-4000));
  if (!externalGateway) {
    assert.equal(requests.filter(p => p === '/v1/images/generations').length, 1, output.slice(-4000));
    assert.equal(requests.filter(p => p === '/v1/images/edits').length, rejectImage ? 0 : 1, output.slice(-4000));
    assert.equal(calls, rejectImage ? 2 : 3, 'no client replay');
  }
  async function imageFiles(dir) {
    let found = [];
    for (const e of await readdir(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) found.push(...await imageFiles(p));
      else if (e.name.endsWith('.png')) found.push(p);
    }
    return found;
  }
  const files = (await imageFiles(root)).filter(p => p.includes('generated_images'));
  assert.ok(rejectImage ? files.length === 0 : files.length === 2, 'native engine must only save successful images');
  for (const file of files) assert.ok((await readFile(file)).equals(Buffer.from(png, 'base64')), 'saved fixture image bytes');
  console.log(JSON.stringify({ version, nativeTool: true, scenario: rejectImage ? 'upstream_500' : 'generate_and_edit', savedImages: files.length, replay: false, isolated: true }));
} finally {
  server.closeAllConnections();
  await new Promise(r => server.close(r));
  // Only delete the exact mkdtemp directory created by this process.
  if (root.startsWith(join(tmpdir(), 'fusion-native-images-'))) {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }).catch(error => {
      // Windows helpers may briefly retain a fixture directory after the engine has exited.
      // Preserve the actual validation result; this directory contains only public fake data.
      console.warn(`Fixture cleanup deferred (${error.code}); retained ${root}`);
    });
  }
}

