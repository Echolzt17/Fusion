// Compile and execute the production offline dependency macro with a harmless fake installer.
// All state is under one mkdtemp directory. Never installs WebView2 or changes the registry.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, writeFile, rm, stat } from 'node:fs/promises';
import { join, resolve, dirname, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
const exec = promisify(execFile);
const workspace = fileURLToPath(new URL('../', import.meta.url));
const compiler = join(process.env.LOCALAPPDATA, 'tauri/NSIS/makensis.exe');
const root = await mkdtemp(join(tmpdir(), 'fusion-webview-中文 空格-'));
const quote = value => value.replaceAll('$', '$$$$').replaceAll('"', '$\\"');
const env = { ...process.env, FUSION_WEBVIEW_FIXTURE: root };
const config = JSON.parse(await readFile(join(workspace, 'src-tauri/tauri.conf.json'), 'utf8'));
const minimum = config.bundle.windows.minimumWebview2Version;
const checker = join(root, 'runtime checker.exe');
const fake = join(root, 'offline fixture.exe'), harness = join(root, 'guard.exe');
let passed = 0;
try {
  assert.equal(config.bundle.windows.webviewInstallMode.type, 'offlineInstaller');
  assert.equal(config.bundle.windows.nsis.installMode, 'currentUser');
  const template = await readFile(join(workspace, 'src-tauri/installer.nsi'), 'utf8');
  assert.doesNotMatch(template, /SetCompressor\s+\/SOLID/i);
  await writeFile(join(root, 'fake.nsi'), `Unicode true
!include LogicLib.nsh
Name "Fusion harmless offline fixture"
OutFile "${quote(fake)}"
RequestExecutionLevel user
SilentInstall silent
Section
  ReadEnvStr $0 FUSION_WEBVIEW_FIXTURE
  FileOpen $1 "$0\\invoked" w
  FileWrite $1 "called"
  FileClose $1
  FileOpen $1 "$0\\outcome-version" r
  FileRead $1 $2
  FileClose $1
  FileOpen $1 "$0\\version" w
  FileWrite $1 $2
  FileClose $1
  FileOpen $1 "$0\\outcome-code" r
  FileRead $1 $2
  FileClose $1
  SetErrorLevel $2
SectionEnd
`);
  await exec(compiler, ['/INPUTCHARSET', 'UTF8', '/V2', join(root, 'fake.nsi')], { windowsHide: true, timeout: 30000 });
  await writeFile(join(root, 'checker.nsi'), `Unicode true
!include LogicLib.nsh
!include WordFunc.nsh
Name "Fusion harmless runtime checker"
OutFile "${quote(checker)}"
RequestExecutionLevel user
SilentInstall silent
Section
  ReadEnvStr $0 FUSION_WEBVIEW_FIXTURE
  StrCpy $2 "0.0.0.0"
  FileOpen $1 "$0\\version" r
  IfErrors checked
  FileRead $1 $2
  FileClose $1
  checked:
  \${VersionCompare} "$2" "${minimum}" $3
  \${If} $3 = 2
    SetErrorLevel 1
  \${Else}
    SetErrorLevel 0
  \${EndIf}
SectionEnd
`);
  await exec(compiler, ['/INPUTCHARSET', 'UTF8', '/V2', join(root, 'checker.nsi')], { windowsHide: true, timeout: 30000 });
  await writeFile(join(root, 'guard.nsi'), `Unicode true
!include LogicLib.nsh
!include WordFunc.nsh
Name "Fusion isolated dependency guard"
OutFile "${quote(harness)}"
RequestExecutionLevel user
SilentInstall silent
!define FUSION_WEBVIEW_CHECKER "${quote(checker)}"
!define WEBVIEW2INSTALLERPATH "${quote(fake)}"
!define MINIMUMWEBVIEW2VERSION "${minimum}"
!include "${quote(join(workspace, 'src-tauri/installer-webview.nsh'))}"
Section
  !insertmacro FusionEnsureWebView2
  FileOpen $0 "${quote(root)}\\completed" w
  FileWrite $0 "ready"
  FileClose $0
SectionEnd
`);
  await exec(compiler, ['/INPUTCHARSET', 'UTF8', '/V2', join(root, 'guard.nsi')], { windowsHide: true, timeout: 30000 });
  for (const flags of [['/S'], ['/S', '/UPDATE']]) {
    for (const [installed, after, code, invoked, allowed] of [
      [minimum, minimum, 0, false, true],
      ['156.0.4314.50', minimum, 0, false, true],
      ['155.0.4283.62', minimum, 0, false, true],
      ['999.0.0.0', minimum, 0, false, true],
      [null, minimum, 0, true, true],
      ['0.0.0.0', minimum, 0, true, true],
      ['109.0.0.0', minimum, 0, true, true],
      [null, '0.0.0.0', 0, true, false],
      [null, minimum, 1603, true, false],
      [null, minimum, -2147219416, true, true],
      [null, '0.0.0.0', -2147219416, true, false],
      [null, minimum, -2147219187, true, true],
      [null, '0.0.0.0', -2147219187, true, false],
      [null, minimum, 3010, true, true],
      [null, '0.0.0.0', 3010, true, false],
    ]) {
      for (const name of ['version', 'completed', 'invoked']) await rm(join(root, name), { force: true });
      if (installed !== null) await writeFile(join(root, 'version'), installed);
      await writeFile(join(root, 'outcome-version'), after);
      await writeFile(join(root, 'outcome-code'), String(code));
      let exit = 0;
      try { await exec(harness, flags, { env, windowsHide: true, timeout: 15000 }); }
      catch (error) { if (error.killed) throw error; exit = error.code; }
      const exists = name => stat(join(root, name)).then(() => true, () => false);
      assert.equal(exit, allowed ? 0 : 3, `${installed}/${after}/${code}/${flags}: exit`);
      assert.equal(await exists('completed'), allowed);
      assert.equal(await exists('invoked'), invoked);
      if (!invoked) assert.equal(await readFile(join(root, 'version'), 'utf8'), installed);
      passed++;
    }
  }
  console.log(JSON.stringify({ passed, productionOfflineMacro: true, realInstalls: 0, realRegistryWrites: 0 }));
} finally {
  if (dirname(resolve(root)) === resolve(tmpdir()) && basename(root).startsWith('fusion-webview-')) {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}
