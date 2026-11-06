// Exact uninstall selection macro; fake cleanup helper, no real Vault/config/data access.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, writeFile, rm, stat } from 'node:fs/promises';
import { join, resolve, dirname, basename } from 'node:path';
import { tmpdir } from 'node:os';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
const exec = promisify(execFile);
const workspace = fileURLToPath(new URL('../', import.meta.url));
const compiler = join(process.env.LOCALAPPDATA, 'tauri/NSIS/makensis.exe');
const root = await mkdtemp(join(tmpdir(), 'fusion-cleanup-中文 空格-'));
const quote = v => v.replaceAll('$', '$$$$').replaceAll('"', '$\\"');
let passed = 0;
try {
  for (const [selected, update, fail] of [[0,0,0], [0,1,0], [1,1,0], [1,0,0], [1,0,1]]) {
    const called = join(root, 'cleanup-called'); const done = join(root, 'continued');
    const helper = join(root, 'helper.exe'); const helperScript = join(root, 'helper.nsi');
    await writeFile(helperScript, `Unicode true
Name "Fake cleanup"
OutFile "${quote(helper)}"
RequestExecutionLevel user
SilentInstall silent
Section
FileOpen $0 "${quote(called)}" w
FileWrite $0 "called"
FileClose $0
SetErrorLevel ${fail}
SectionEnd
`);
    await exec(compiler, ['/INPUTCHARSET','UTF8','/V2',helperScript], {windowsHide:true});
    const output = join(root, 'case.exe'); const script = join(root, 'case.nsi');
    await writeFile(script, `Unicode true
!include LogicLib.nsh
!include "${quote(join(workspace,'src-tauri/installer-hooks.nsh'))}"
Name "Isolated selection"
OutFile "${quote(output)}"
RequestExecutionLevel user
SilentInstall silent
Var DeleteAppDataCheckboxState
Var UpdateMode
Section
InitPluginsDir
File /oname=$PLUGINSDIR\\fusion-maintenance.exe "${quote(helper)}"
StrCpy $DeleteAppDataCheckboxState ${selected}
StrCpy $UpdateMode ${update}
!insertmacro FusionDeleteAppData
FileOpen $0 "${quote(done)}" w
FileWrite $0 "done"
FileClose $0
SectionEnd
`);
    await exec(compiler, ['/INPUTCHARSET','UTF8','/V2',script], {windowsHide:true});
    let code = 0;
    try { await exec(output, ['/S'], {windowsHide:true, timeout:15000}); } catch(e) { code = e.code; }
    const shouldCall = selected === 1 && update !== 1;
    assert.equal(await stat(called).then(()=>true,()=>false), shouldCall);
    assert.equal(code, shouldCall && fail ? 3 : 0);
    assert.equal(await stat(done).then(()=>true,()=>false), !(shouldCall && fail));
    await rm(called,{force:true}); await rm(done,{force:true}); passed++;
  }
  const template = await readFile(join(workspace,'src-tauri/installer.nsi'),'utf8');
  assert.ok(template.indexOf('!insertmacro NSIS_HOOK_PREUNINSTALL') < template.indexOf('Delete "$INSTDIR\\${MAINBINARYNAME}.exe"'));
  console.log(JSON.stringify({passed, productionCleanupMacro:true, realCredentialsUsed:false, installsPerformed:0}));
} finally {
  if (dirname(resolve(root)) === resolve(tmpdir()) && basename(root).startsWith('fusion-cleanup-')) await rm(root,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
