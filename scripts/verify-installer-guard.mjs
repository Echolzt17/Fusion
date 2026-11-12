// Exercise the exact production NSIS guard without installing, touching registry, or using real accounts.
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, readFile, writeFile, copyFile, rm, stat } from 'node:fs/promises';
import { join, resolve, dirname, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';
import assert from 'node:assert/strict';
const exec = promisify(execFile);
const workspace = fileURLToPath(new URL('../', import.meta.url));
const binary = resolve(process.argv[2] || join(workspace, 'target/x86_64-pc-windows-msvc/release/personal-codex-pool.exe'));
const compiler = join(process.env.LOCALAPPDATA, 'tauri/NSIS/makensis.exe');
const root = await mkdtemp(join(tmpdir(), 'fusion-guard-中文 空格-'));
const home = join(root, 'codex');
const env = { ...process.env, CODEX_HOME: home, LOCALAPPDATA: join(root, 'local') };
const quote = value => value.replaceAll('$', '$$$$').replaceAll('"', '$\\"');
let fixture;
let passed = 0;
try {
  await mkdir(home);
  const config = join(home, 'config.toml');
  const auth = join(home, 'auth.json');
  await writeFile(auth, 'public fixture only');
  for (const mode of ['install', 'uninstall']) {
    const install = join(root, mode); await mkdir(install);
    const target = join(install, 'personal-codex-pool.exe');
    const marker = join(install, 'guard-passed');
    const output = join(root, `guard-${mode}.exe`);
    const script = join(root, `guard-${mode}.nsi`);
    await writeFile(script, `Unicode true
!include LogicLib.nsh
!include "Win\\RestartManager.nsh"
!define MAINBINARYNAME "personal-codex-pool"
!define MAINBINARYSRCPATH "${quote(binary)}"
!include "${quote(join(workspace, 'src-tauri/installer-hooks.nsh'))}"
Name "Fusion isolated guard"
OutFile "${quote(output)}"
RequestExecutionLevel user
SilentInstall silent
InstallDir "${quote(install)}"
Section
  !insertmacro FusionMaintenanceCheck "$INSTDIR" ${mode}
  FileOpen $0 "$INSTDIR\\guard-passed" w
  FileWrite $0 "passed"
  FileClose $0
SectionEnd
`);
    await exec(compiler, ['/INPUTCHARSET', 'UTF8', '/V2', script], { windowsHide: true, timeout: 30000 });
    async function check(content, allowed) {
      await writeFile(config, content);
      await rm(marker, { force: true });
      let code = 0;
      try { await exec(output, ['/S', '/UPDATE'], { env, windowsHide: true, timeout: 15000 }); }
      catch (error) { if (error.killed) throw error; code = error.code; }
      assert.equal(code === 0, allowed, `${mode}: unexpected exit ${code}`);
      assert.equal(await stat(marker).then(()=>true,()=>false), allowed, `${mode}: guard marker`);
      assert.ok((await readFile(config)).equals(Buffer.from(content)), 'guard must not write configuration');
      assert.equal(await readFile(auth,'utf8'), 'public fixture only');
      passed++;
    }
    if (mode === 'install') {
      await check('model_provider="openai"\n', true);
      await check('model_provider="personal_codex_pool"\n', false);
    }
    await copyFile(binary, target);
    await check('model_provider="openai"\n', true);
    await check('model_provider="personal_codex_pool"\n', false);
    await check('profile="work"\n[profiles.work]\nmodel_provider="personal_codex_pool"\n', false);
    await check('model_provider="openai"\n[model_providers.personal_codex_pool]\nname="unused"\n', true);
    await check('broken = [\n', false);
    if (mode === 'install') {
      // An old binary that does not implement Fusion CLI switches must never be executed as the checker.
      await copyFile(process.env.ComSpec, target);
      await check('model_provider="openai"\n', true);
      fixture = spawn(target, ['/d', '/c', 'set /p fusion_guard_fixture='], { env, cwd: install, windowsHide: true, stdio: ['pipe','ignore','ignore'] });
      await new Promise(r=>setTimeout(r,350));
      assert.equal(fixture.exitCode, null, 'fixture must be running');
      await check('model_provider="openai"\n', false);
      assert.equal(fixture.exitCode, null, 'guard must not terminate the running process');
      const exited = once(fixture,'close');fixture.stdin.end('\n');await exited;fixture=null;
    }
  }
  // The updater starts its installer just before exiting. Exercise that race
  // using only a local, harmless process and temporary config paths.
  const waitScript = join(root, 'wait-update.nsi');
  const waitOutput = join(root, 'wait-update.exe');
  const waitInstall = join(root, 'install');
  const waitTarget = join(waitInstall, 'personal-codex-pool.exe');
  const source = await readFile(join(root, 'guard-install.nsi'), 'utf8');
  await writeFile(waitScript, source.replace(join(root, 'guard-install.exe'), waitOutput).replace('!insertmacro FusionMaintenanceCheck "$INSTDIR" install', '!insertmacro FusionWaitForUpdateExit "$INSTDIR"\n  !insertmacro FusionMaintenanceCheck "$INSTDIR" install'));
  await exec(compiler, ['/INPUTCHARSET','UTF8','/V2',waitScript], {windowsHide:true,timeout:30000});
  await writeFile(join(home,'config.toml'), 'model_provider="openai"\n');
  await copyFile(process.env.ComSpec, waitTarget);
  const startFixture = () => spawn(waitTarget, ['/d','/c','set /p fusion_wait_fixture='], {env,cwd:waitInstall,windowsHide:true,stdio:['pipe','ignore','ignore']});
  fixture = startFixture(); await new Promise(r=>setTimeout(r,350));
  // Production performs at most 100 polls. RmGetList is an OS call whose time
  // is additional to the 100 ms sleeps; a 20 s test budget was too close on a
  // loaded machine. Keep a finite budget and the exit/not-killed assertions.
  const updateWaitTimeout = 60000;
  const pendingWait = exec(waitOutput, ['/S','/UPDATE'], {env,windowsHide:true,timeout:updateWaitTimeout});
  await new Promise(r=>setTimeout(r,600)); assert.equal(fixture.exitCode,null);
  { const exited=once(fixture,'close');fixture.stdin.end('\n');await exited;fixture=null; }
  await pendingWait;passed++;
  fixture=startFixture();await new Promise(r=>setTimeout(r,350));
  const boundedWaitStarted = performance.now();
  await assert.rejects(exec(waitOutput,['/S','/UPDATE'],{env,windowsHide:true,timeout:updateWaitTimeout}),error=>error.code===2&&!error.killed);
  const boundedWaitElapsedMs = Math.round(performance.now() - boundedWaitStarted);
  assert.equal(fixture.exitCode,null,'bounded wait must never kill an active process');
  { const exited=once(fixture,'close');fixture.stdin.end('\n');await exited;fixture=null; }passed++;
  // Exercise the real no-database runtime helper from a Unicode/space path.
  const runtimeHelper = join(root, '运行时 helper.exe');
  await copyFile(binary, runtimeHelper);
  let runtimeReady = true;
  try {
    const result = await exec(runtimeHelper, ['--check-webview-runtime'], { env, windowsHide: true, timeout: 15000 });
    assert.equal(result.stdout, ''); assert.equal(result.stderr, '');
  } catch (error) {
    assert.equal(error.code, 1); assert.equal(error.stdout, ''); assert.equal(error.stderr, '');
    runtimeReady = false;
  }
  passed++;
  // Compile the exact page policy used by the production template. Abort skips
  // this page, not the installer; no registry or installed application is touched.
  const template = await readFile(join(workspace, 'src-tauri/installer.nsi'), 'utf8');
  assert.match(template, /Pop \$R0\s+!insertmacro FusionUpgradeChoice \$R0 \$WixMode \$PassiveMode \$R1\s+\$\{If\} \$R1 = 0\s+Abort/);
  const choiceScript = join(root, 'upgrade-choice.nsi');
  const choiceOutput = join(root, 'upgrade-choice.exe');
  await writeFile(choiceScript, `Unicode true
!include LogicLib.nsh
!include "${quote(join(workspace, 'src-tauri/installer-hooks.nsh'))}"
Name "Fusion isolated upgrade choice"
OutFile "${quote(choiceOutput)}"
RequestExecutionLevel user
SilentInstall silent
Var Comparison
Var Wix
Var Passive
Function MaintenancePage
  !insertmacro FusionUpgradeChoice $Comparison $Wix $Passive $0
FunctionEnd
Section
  StrCpy $Wix 0
  StrCpy $Passive 0
  StrCpy $Comparison 1
  StrCpy $0 0
  Call MaintenancePage
  IntCmp $0 0 +2
  Goto failed
  StrCpy $Comparison 0
  Call MaintenancePage
  IntCmp $0 1 +2
  Goto failed
  StrCpy $Passive 1
  Call MaintenancePage
  IntCmp $0 0 +2
  Goto failed
  StrCpy $Comparison -1
  StrCpy $0 0
  Call MaintenancePage
  IntCmp $0 1 +2
  Goto failed
  StrCpy $Comparison 1
  StrCpy $Wix 1
  StrCpy $0 0
  Call MaintenancePage
  IntCmp $0 1 +2
  Goto failed
  SetErrorLevel 0
  Goto done
  failed:
  SetErrorLevel 9
  done:
SectionEnd
`);
  await exec(compiler, ['/INPUTCHARSET','UTF8','/V2',choiceScript], {windowsHide:true,timeout:30000});
  await exec(choiceOutput, ['/S'], {env,windowsHide:true,timeout:15000});
  passed += 5;
  assert.equal(await stat(join(env.LOCALAPPDATA,'PersonalCodexPool/pool.sqlite')).then(()=>true,()=>false), false, 'checker must not initialize a database');
  console.log(JSON.stringify({ passed, productionGuard: true, installsPerformed: 0, realConfigUsed: false, runningFixtureTerminated: false, boundedWaitElapsedMs, runtimeHelperChecked: true, runtimeReady }));
} finally {
  if (fixture?.exitCode === null) { const exited=once(fixture,'close');fixture.stdin.end('\n');await exited; }
  // Only this exact mkdtemp child of the system temp directory may be removed.
  if (dirname(resolve(root)) === resolve(tmpdir()) && basename(root).startsWith('fusion-guard-')) await rm(root,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
