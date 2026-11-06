import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, failures: string[] = [], enabled = true, slowReset = false) {
  await page.addInitScript(({ failures, enabled, slowReset }) => {
    const state = {
      calls: [] as { command: string; args: Record<string, unknown> }[], failures,
      mode: enabled ? 'on' : 'off',
      service: { running: enabled, draining: false, activeRequests: 2, baseUrl: 'http://127.0.0.1:18765/v1' },
      applied: enabled,
      visible: true,
      login: { phase: 'idle', message: '' },
      reloadRequired: null as string | null,
      phase: null as string | null, error: null as string | null,
      operation: null as string | null,
      logs: [{ id: 'fixture-image', at: 1, account: 'fixture-a', operation: 'image_edit', status: 'success', elapsedMs: 1, advice: '', conversationKey: 'thread:fixture-hash' }],
      accounts: [{ id: 'fixture-a', available: true, email: 'fixture@example.test', state: 'ready', cooldownUntil: 0, planLabel: 'Plus', quota: null, quotaError: null }],
      listeners: {} as Record<string, number>, callbacks: {} as Record<number, (event: unknown) => void>,
    };
    Object.assign(window, { fixture: state, __TAURI_INTERNALS__: {
      transformCallback: (callback: (event: unknown) => void) => { const id = Object.keys(state.callbacks).length + 1; state.callbacks[id] = callback; return id; },
      invoke: async (command: string, args: Record<string, unknown> = {}) => {
        state.calls.push({ command, args });
        if (state.failures.includes(command)) throw new Error(`fixture failure: ${command}`);
        if (command === 'plugin:event|listen') { state.listeners[String(args.event)] = Number(args.handler); return 1; }
        if (command === 'plugin:event|unlisten') return;
        switch (command) {
          case 'update_status': return {channel:'stable',currentVersion:'0.1.30',phase:'current',version:null,notes:null,downloaded:0,total:null,message:null,installing:false};
          case 'uninstall_requested': return false;
          case 'main_visible': return state.visible;
          case 'settings': return { port: 18765, concurrency: 2, fixedAccount: null, selectionPolicy: 'auto' };
          case 'accounts': return state.accounts;
          case 'api_client_status': return [];
          case 'service_status': return state.service;
          case 'cancel_request': {
            const service = state.service as typeof state.service & { requests?: { id: string }[] };
            service.requests = (service.requests || []).filter(request => request.id !== args.id);
            return;
          }
          case 'login_status': return state.login;
          case 'logs': return state.logs;
          case 'conversation_usage': return {conversations:[]};
          case 'request_history': return {items:state.logs.filter(l=>(args.system ? l.operation==='models' : l.operation!=='models') && (!args.conversationKey || l.conversationKey===args.conversationKey)),nextCursor:null};
          case 'usage': return [];
          case 'usage_dashboard': return null;
          case 'connection_status': return { mode: state.mode, configured: state.applied, recoveryPending: state.applied, reloadRequired: state.reloadRequired, error: state.error, operation: state.operation, phase: state.phase, operationStartedAt: Math.floor(Date.now() / 1000) };
          case 'scheduling_status': return { policy: 'auto', currentAccountId: 'fixture-a', preferredAccountId: null };
          case 'image_compatibility': return { client: { desktop: '26.1027.3384.0', engine: 'codex-cli 0.166.0-alpha.5.1' }, verificationState: state.phase === 'compatibility' ? 'checking' : 'verified', detail: state.phase === 'compatibility' ? '正在本机临时环境检查对话、生图、改图与失败不重放，不使用账号或真实配置' : '当前客户端已通过本机隔离协议检查，实际桌面生图仍需验收', isolatedVerified: state.phase !== 'compatibility', configurationReady: true, routesReady: state.service.running, upstreamImageReceived: false, desktopAcceptance: 'pending' };
          case 'refresh_quota': return;
          case 'reset_credits': if (slowReset) return new Promise(() => {}); return { available_count: 1, credits: [{ id: 'fixture-card', reset_type: 'codex_rate_limits', status: 'available', expires_at: null }] };
          case 'remove_account': state.accounts = []; return;
          case 'restore_preview': return { path: 'temporary-fixture/config.toml', before: state.applied ? 'pool provider' : '', after: '', fingerprint: 'restore-fingerprint', strategy: 'normal', archivePath: null, warning: null, canApply: true };
          case 'start_pool': state.mode = 'enabling'; state.operation = 'start'; state.phase = 'checking'; return;
          case 'close_pool': state.service.draining = true; state.mode = 'closing'; state.operation = 'stop'; state.phase = 'draining'; state.applied = false; return;
          case 'consume_reset': return 'fixture consumed';
          default: throw new Error(`Unexpected fixture IPC: ${command}`);
        }
      },
    } });
  }, { failures, enabled, slowReset });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'fixture@example.test' })).toBeVisible();
}

async function reportingFixture(page: Page, empty=false, failOnce=false) {
  await fixture(page);
  await page.evaluate(({empty,failOnce})=>{
    const w=window as any;const invoke=w.__TAURI_INTERNALS__.invoke;let submissions=0;
    let preview={draftId:'report-fixture',errorCount:6,summaryRequests:100,summarySuccess:94,summaryFailure:6,summaryRecovered:3,summaryPartial:true,rangeFrom:1793500000,rangeTo:1793670000,compressedBytes:4096,historyLimited:true,destination:'https://162.14.80.7/v1/error-reports',categories:{upstream_error:6} as Record<string,number>};
    w.__TAURI_INTERNALS__.invoke=async(command:string,args:any={})=>{
      if(command==='prepare_error_report'){w.fixture.calls.push({command,args});preview={...preview,errorCount:args.summaryOnly?0:6,categories:args.summaryOnly?{}:{upstream_error:6}};return {phase:empty?'empty':'prepared',message:empty?'暂无可上报错误':null,preview:empty?null:preview,receipt:null};}
      if(command==='submit_error_report'){w.fixture.calls.push({command,args});submissions++;return failOnce&&submissions===1?{phase:'failed',preview,message:'连接超时，接收状态未确认',receipt:null}:{phase:'sent',preview,message:'错误日志上报成功',receipt:{reportId:'report-fixture',acceptedErrors:0,duplicateErrors:6}};}
      if(command==='save_error_report'){w.fixture.calls.push({command,args});return 'temporary-fixture/error-report.json.gz';}
      if(command==='error_report_status'){w.fixture.calls.push({command,args});return {phase:'idle',preview:null,receipt:null,history:[{reportId:'00000000-0000-4000-8000-000000000001',receivedAt:1793682001,acceptedErrors:7,duplicateErrors:6}],historyError:null};}
      return invoke(command,args);
    };
  },{empty,failOnce});
  await page.getByRole('button',{name:/请求日志/}).first().click();
}

async function pendingQuotaFixture(page: Page, enabled = true) {
  await fixture(page, [], enabled);
  await page.evaluate(() => {
    const w = window as any, state = w.fixture;
    state.accounts.push({ ...state.accounts[0], id: 'fixture-b', email: 'second@example.test' });
    state.quotaPending = {};
    const invoke = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
      if (command === 'refresh_quota') {
        state.calls.push({ command, args });
        return new Promise<void>((resolve, reject) => { state.quotaPending[args.id] = { resolve, reject }; });
      }
      if (command === 'pause_account') {
        state.calls.push({ command, args });
        state.accounts.find((a: any) => a.id === args.id).state = args.paused ? 'paused' : 'ready';
        return;
      }
      return invoke(command, args);
    };
    state.callbacks[state.listeners['main-visible']]({ payload: true });
  });
  await expect(page.getByRole('heading', { name: 'second@example.test' })).toBeVisible();
}

test('slow quota reads leave lifecycle and other account actions usable while writes remain guarded', async ({ page }) => {
  await pendingQuotaFixture(page);
  const first = page.locator('.account').filter({ hasText: 'fixture@example.test' });
  const second = page.locator('.account').filter({ hasText: 'second@example.test' });
  await first.getByRole('button', { name: '刷新额度', exact: true }).click();
  await expect(first.getByRole('button', { name: '刷新额度', exact: true })).toHaveAttribute('aria-busy', 'true');
  await expect(first.getByRole('button', { name: '刷新额度', exact: true })).toBeDisabled();
  await expect(first.locator('.quota-refreshing')).toContainText('更新中');
  await expect(second.getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
  await second.getByRole('button', { name: '暂停账号', exact: true }).click();
  await expect(second.getByRole('button', { name: '恢复账号', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: '关闭账号池', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '关闭账号池', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.shutdown-progress')).toBeVisible();
  await page.getByRole('button', { name: '账号控制台', exact: true }).click();
  await expect(second.getByRole('button', { name: '移除账号', exact: true })).toBeDisabled();
  await expect(second.getByRole('button', { name: '恢复账号', exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'refresh_quota').length)).toBe(1);
});

test('refresh all joins account reads and reports partial failure only after every read settles', async ({ page }) => {
  await pendingQuotaFixture(page, false);
  const first = page.locator('.account').filter({ hasText: 'fixture@example.test' });
  const all = page.getByRole('button', { name: '刷新全部额度', exact: true });
  await first.getByRole('button', { name: '刷新额度', exact: true }).click();
  await all.click();
  await expect(all).toHaveAttribute('aria-busy', 'true');
  await expect(all).toBeDisabled();
  await expect(page.getByRole('button', { name: '启动账号池', exact: true })).toBeEnabled();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'refresh_quota').map((c: any) => c.args.id))).toEqual(['fixture-a', 'fixture-b']);
  await page.evaluate(() => (window as any).fixture.quotaPending['fixture-b'].reject('fixture quota unavailable'));
  await expect(page.locator('.account').filter({ hasText: 'second@example.test' }).getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
  await expect(all).toHaveAttribute('aria-busy', 'true');
  await expect(page.getByRole('button', { name: /通知中心/ })).toHaveAttribute('aria-label', '通知中心，0 条未读');
  await page.evaluate(() => (window as any).fixture.quotaPending['fixture-a'].resolve());
  await expect(all).toBeEnabled();
  await expect(all).toHaveAttribute('aria-busy', 'false');
  await page.getByRole('button', { name: /通知中心/ }).click();
  await page.locator('.notification-row').filter({ hasText: '额度刷新完成，部分账号未更新' }).click();
  await expect(page.getByRole('dialog')).toContainText('成功 1 个，失败 1 个');
  await expect(page.getByRole('dialog')).toContainText('失败账号保留上次结果');
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'refresh_quota').length)).toBe(2);
});

test('lifecycle controls stay on the dashboard without redundant guidance or settings entry', async ({ page }) => {
  await fixture(page);
  await expect(page.getByLabel('聊天续接说明')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '关闭账号池', exact: true })).toHaveCount(1);
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.getByLabel('聊天续接说明')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'Codex 接入', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '关闭账号池', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '检查连接', exact: true })).toHaveCount(0);
});
test('error report requires explicit confirmation and cancel sends nothing',async({page})=>{
  await reportingFixture(page);const trigger=page.getByRole('button',{name:'日志上报',exact:true});await trigger.click();
  const dialog=page.getByRole('dialog',{name:'确认上报错误日志'});await expect(dialog).toContainText('6 条');await expect(dialog).toContainText('162.14.80.7');
  await expect(dialog).toContainText('不含逐条成功');await page.screenshot({path:'test-results/error-report-light-confirm.png'});await dialog.getByRole('button',{name:'取消',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='submit_error_report').length)).toBe(0);
  await expect(trigger).toBeFocused();await trigger.click();await dialog.getByRole('button',{name:'确认上报',exact:true}).click();await expect(dialog).toContainText('报告编号：report-fixture');
});
test('numeric summary can be selected without submitting individual errors',async({page})=>{
  await reportingFixture(page);await page.getByRole('button',{name:'日志上报',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'确认上报错误日志'});
  await dialog.getByLabel('仅上报统计摘要').check();
  await expect(dialog).toContainText('统计成功 94');await expect(dialog).toContainText('发送前恢复后完成 3');
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='prepare_error_report').at(-1).args.summaryOnly)).toBe(true);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='submit_error_report').length)).toBe(0);
});
test('empty error history never submits or opens confirmation',async({page})=>{
  await reportingFixture(page,true);await page.getByRole('button',{name:'日志上报',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'确认上报错误日志'})).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='submit_error_report').length)).toBe(0);
});

test('recent reports remain available independently of a new preview without submitting',async({page})=>{
  await reportingFixture(page,true);
  await page.setViewportSize({width:700,height:650});
  const trigger=page.getByRole('button',{name:'最近上报',exact:true});await trigger.click();
  const dialog=page.getByRole('dialog',{name:'最近上报',exact:true});
  await expect(dialog).toContainText('13 条错误');await expect(dialog).toContainText('新增 7');
  await dialog.getByText('查看回执',{exact:true}).click();
  await expect(dialog).toContainText('00000000-0000-4000-8000-000000000001');
  await page.screenshot({path:'test-results/recent-error-reports.png'});
  await dialog.getByRole('button',{name:'完成',exact:true}).click();await expect(trigger).toBeFocused();
  await trigger.click();await expect(dialog).toContainText('13 条错误');
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>['prepare_error_report','submit_error_report'].includes(c.command)).length)).toBe(0);
});
test('failed report retries same preview and can save sanitized bundle in dark narrow window',async({page})=>{
  await reportingFixture(page,false,true);await page.setViewportSize({width:700,height:650});await page.evaluate(()=>{document.documentElement.dataset.theme='dark';});
  await page.getByRole('button',{name:'日志上报',exact:true}).click();const dialog=page.getByRole('dialog',{name:'确认上报错误日志'});
  await dialog.getByRole('button',{name:'确认上报',exact:true}).click();await expect(dialog.getByRole('alert')).toContainText('接收状态未确认');
  await dialog.getByRole('button',{name:'保存脱敏错误包'}).click();await dialog.getByRole('button',{name:'重试同一报告'}).click();await expect(dialog).toContainText('错误日志上报成功');
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='submit_error_report').map((c:any)=>c.args.draftId))).toEqual(['report-fixture','report-fixture']);
  await page.screenshot({path:'test-results/error-report-dark.png'});
});

test('optional IPC failures leave account/service controls usable and recover next poll', async ({ page }) => {
  await fixture(page, ['connection_status', 'usage', 'image_compatibility']);
  await expect(page.locator('.local-tag')).toBeVisible();
  await page.getByRole('button', { name: /通知中心/ }).click();
  await expect(page.getByRole('region', { name: '通知列表' })).toContainText('账号池接入读取失败');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '移除账号', exact: true }).click();
  await expect(page.getByRole('dialog', { name: '确认操作' })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.evaluate(() => { (window as any).fixture.failures = []; });
  await expect(page.getByRole('button', { name: '关闭账号池', exact: true })).toBeEnabled({ timeout: 8000 });
  await page.getByRole('button', { name: /通知中心/ }).click();
  await expect(page.getByRole('region', { name: '通知列表' })).toContainText('账号池接入读取失败');
  await expect(page.locator('.notification-row').filter({ hasText: '账号池接入读取失败' })).toHaveCount(1);
});

test('modal traps keyboard, prevents background focus, restores opener, and cancel has no effect', async ({ page }) => {
  await fixture(page);
  const opener = page.getByRole('button', { name: '移除账号', exact: true });
  await opener.click();
  const dialog = page.getByRole('dialog', { name: '确认操作' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('关联的池内旧聊天将无法继续');
  await expect(dialog).toContainText('重新添加同一账号也不会恢复这些会话绑定');
  await expect(dialog).toContainText('暂停账号');
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press(i % 2 ? 'Shift+Tab' : 'Tab');
    expect(await page.evaluate(() => !!document.activeElement?.closest('dialog'))).toBe(true);
  }
  await page.evaluate(() => (document.querySelector('nav button') as HTMLElement).focus());
  expect(await page.evaluate(() => !!document.activeElement?.closest('dialog'))).toBe(true);
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(opener).toBeFocused();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'remove_account').length)).toBe(0);
  await opener.click(); await page.getByRole('button', { name: '确认移除账号' }).click();
  await expect(page.getByRole('heading', { name: '尚无账号' })).toBeVisible();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'remove_account').length)).toBe(1);
});

test('one click starts without a config preview and reset card still requires explicit selection', async ({ page }) => {
  await fixture(page, [], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'start_pool').map((c: any)=>c.args))).toEqual([{accountId:null}]);
  expect(await page.evaluate(() => (window as any).fixture.calls.some((c: any) => ['config_preview','config_apply','restore_preview'].includes(c.command)))).toBe(false);
  await page.evaluate(() => { const s=(window as any).fixture; s.mode='on';s.applied=true;s.service.running=true;s.operation=null;s.phase=null;s.callbacks[s.listeners['start-complete']]({payload:null}); });
  await expect(page.locator('.local-tag')).toHaveText('账号池已启动');
  await page.getByRole('button', { name: '重置额度', exact: true }).click();
  await expect(page.getByRole('button', { name: '确认重置额度' })).toBeDisabled();
  await page.getByLabel('选择重置卡').selectOption('fixture-card');
  await page.getByRole('button', { name: '确认重置额度' }).click();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'consume_reset').length)).toBe(1);
});

test('close starts immediately without strategy choices and displays progress', async ({ page }) => {
  await fixture(page);
  await page.getByRole('button', { name: '关闭账号池', exact: true }).click();
  await expect(page.getByRole('dialog', { name: '停止与退出' })).toHaveCount(0);
  await expect(page.locator('.shutdown-progress')).toContainText('活动请求 2 个');
  await expect(page.getByRole('button', { name: /通知中心/ })).toHaveAttribute('aria-label', '通知中心，0 条未读');
  await expect(page.getByRole('button', { name: '正在关闭…', exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'close_pool').map((c: any) => c.args))).toEqual([{ exitApp: false }]);
  await page.evaluate(() => { const s = (window as any).fixture; s.mode='off'; s.phase=null; s.service.running=false; s.service.activeRequests=0; s.callbacks[s.listeners['close-complete']]({payload:null}); s.callbacks[s.listeners['main-visible']]({payload:true}); });
  await expect(page.locator('.local-tag')).toHaveText('账号池已关闭');
  await expect(page.locator('.shutdown-progress')).toHaveCount(0);
  await expect(page.getByRole('button', { name: /通知中心/ })).toHaveAttribute('aria-label', '通知中心，1 条未读');
  await expect(page.getByRole('button', { name: '启动账号池', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: /通知中心/ }).click();
  await page.locator('.notification-row').click();
  await expect(page.getByRole('dialog')).toContainText('原接入已恢复，Codex 已关闭');
  await expect(page.getByRole('dialog')).toContainText('继续池内旧聊天需重新启动账号池并保留原账号');
  await expect(page.getByRole('dialog')).toContainText('新建聊天并带入任务摘要');
});

test('availability comes from core and image operation is labelled', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { (window as any).fixture.accounts[0].available = false; });
  await expect(page.getByText('0 / 1', { exact: true })).toBeVisible({ timeout: 8000 });
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.getByRole('heading', { name: '原生生图', exact: true })).toBeVisible();
  await expect(page.getByRole('group', { name: 'Codex 输入框位置示意' })).toHaveCount(0);
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  await expect(page.getByText('图片编辑', { exact: true })).toBeVisible();
});

test('uninstall close event invokes the same automatic close without confirmation', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { const s=(window as any).fixture; s.callbacks[s.listeners['prepare-uninstall']]({payload:null}); });
  await expect(page.locator('.shutdown-progress')).toBeVisible();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='close_pool').length)).toBe(1);
});

test('backend switch continues without a modal while navigating pages', async ({ page }) => {
  await fixture(page, [], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.locator('.shutdown-progress')).toContainText('正在检查客户端');
  await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: '账号控制台', exact: true }).click();
  await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toBeDisabled();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'start_pool').length)).toBe(1);
});

test('new desktop versions show automatic compatibility progress without manual version gates', async ({ page }) => {
  await fixture(page, [], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  await page.evaluate(() => { const s=(window as any).fixture; s.phase='compatibility'; s.callbacks[s.listeners['main-visible']]({payload:true}); });
  await expect(page.locator('.shutdown-progress')).toContainText('正在自动检查客户端兼容性');
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.getByText('桌面 26.1027.3384.0 · codex-cli 0.166.0-alpha.5.1', {exact:true})).toBeVisible();
  await expect(page.getByText(/正在本机临时环境检查对话、生图、改图/)).toBeVisible();
  await expect(page.getByText(/暂不开放配置升级|请先打开已验证/)).toHaveCount(0);
  await page.getByRole('button', { name: '账号控制台', exact: true }).click();
  await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c:any) => c.command==='start_pool').length)).toBe(1);
  await page.evaluate(() => { const s=(window as any).fixture; s.phase='failed';s.mode='off';s.operation='start';s.error='客户端自动兼容检查未通过：图片编辑协议不兼容。桌面 26.1027.3384.0 · codex-cli 0.166.0-alpha.5.1';s.callbacks[s.listeners['start-failed']]({payload:s.error}); });
  await expect(page.getByRole('alert')).toContainText('图片编辑协议不兼容');
  await expect(page.getByRole('alert')).toContainText('0.166.0-alpha.5.1');
});

test('start admission failure is directly visible and retry uses one backend command', async ({ page }) => {
  await fixture(page, ['start_pool'], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('fixture failure: start_pool');
  await expect(page.getByRole('button', { name: '重试启动', exact: true })).toBeEnabled();
  await page.evaluate(() => { (window as any).fixture.failures = []; });
  await page.getByRole('button', { name: '重试启动', exact: true }).click();
  await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toBeDisabled();
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('dashboard shows drain, force exit and verification without premature success', async ({ page }) => {
  await fixture(page, [], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  for (const [phase,text] of [['catalog_queue','等待账号名额'],['catalog_auth','正在检查或续期授权'],['catalog_proxy','正在解析当前代理'],['catalog_headers','等待上游模型目录响应'],['catalog_body','正在读取模型目录'],['draining','等待已有请求完成'],['terminating','正在结束 Codex 后台'],['verifying','正在核验客户端']]) {
    await page.evaluate(phase => { const s=(window as any).fixture; s.applied=true;s.service.running=true;s.phase=phase;s.callbacks[s.listeners['main-visible']]({payload:true}); },phase);
    await expect(page.locator('.shutdown-progress')).toContainText(text);
    await expect(page.locator('.local-tag')).toHaveText('正在启动账号池');
    await expect(page.getByRole('button', { name: '正在启动…', exact: true })).toBeDisabled();
  }
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c:any) => c.command==='start_pool').length)).toBe(1);
});

test('failed client launch offers start retry and successful close clears its error', async ({ page }) => {
  await fixture(page, [], false);
  await page.getByRole('button', { name: '启动账号池', exact: true }).click();
  await page.evaluate(() => { const s=(window as any).fixture; s.mode='recovery_required';s.operation='start';s.phase='failed';s.applied=true;s.error='fixture client launch failed';s.callbacks[s.listeners['start-failed']]({payload:s.error}); });
  await expect(page.getByRole('alert')).toContainText('fixture client launch failed');
  await expect(page.getByRole('button', { name: '重试启动', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '重试启动', exact: true }).click();
  await page.evaluate(() => { const s=(window as any).fixture; s.mode='on';s.operation=null;s.phase=null;s.error=null;s.service.running=true;s.callbacks[s.listeners['start-complete']]({payload:null}); });
  await expect(page.getByRole('alert')).toHaveCount(0);
  await page.getByRole('button', { name: '关闭账号池', exact: true }).click();
  await page.evaluate(() => { const s=(window as any).fixture; s.mode='off';s.operation=null;s.phase=null;s.applied=false;s.service.running=false;s.callbacks[s.listeners['close-complete']]({payload:null}); });
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('no available accounts explains disabled enable but still allows recovery close', async ({ page }) => {
  await fixture(page, [], false);
  await page.evaluate(() => { const s = (window as any).fixture; s.accounts = []; s.callbacks[s.listeners['main-visible']]({payload:true}); });
  await expect(page.getByRole('button', { name: '启动账号池', exact: true })).toBeDisabled();
  await expect(page.getByText('请先添加账号，再启动账号池。', { exact: true })).toBeVisible();
  await page.evaluate(() => { const s = (window as any).fixture; s.mode = 'recovery_required'; s.callbacks[s.listeners['main-visible']]({payload:true}); });
  await expect(page.getByRole('button', { name: '关闭账号池', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '关闭账号池', exact: true }).click();
  expect(await page.evaluate(() => (window as any).fixture.calls.some((c: any) => c.command === 'close_pool'))).toBe(true);
});

test('failed desktop exit shows one retry and keeps the requested exit target', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const s=(window as any).fixture; s.mode='recovery_required';s.operation='stop';s.phase='failed';s.error='fixture desktop blocked';
    s.callbacks[s.listeners['close-failed']]({payload:{message:s.error,exitApp:true}});
    s.callbacks[s.listeners['main-visible']]({payload:true});
  });
  await expect(page.getByRole('button',{name:'重试关闭',exact:true})).toBeVisible();
  await expect(page.getByRole('button', { name: /通知中心/ })).toHaveAttribute('aria-label', '通知中心，1 条未读');
  await page.getByRole('button', { name: /通知中心/ }).click();
  await expect(page.locator('.notification-row')).toHaveCount(1);
  await expect(page.locator('.notification-row')).toContainText('关闭未完成');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByRole('button',{name:/强制|仅退出|救援/})).toHaveCount(0);
  await page.getByRole('button',{name:'重试关闭',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).fixture.calls.find((c:any)=>c.command==='close_pool').args)).toEqual({exitApp:true});
});

test('first account display queries reset cards once and hidden main window stops detail polling', async ({ page }) => {
  await fixture(page);
  await expect(page.getByText('可用重置卡 1 张', {exact:true})).toBeVisible();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'reset_credits').length)).toBe(1);
  expect(await page.evaluate(() => (window as any).fixture.calls.some((c: any) => c.command === 'logs'))).toBe(true);
  await page.evaluate(() => { (window as any).fixture.visible = false; });
  const count = await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => ['accounts','usage','connection_status'].includes(c.command)).length);
  await page.waitForTimeout(3400);
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => ['accounts','usage','connection_status'].includes(c.command)).length)).toBe(count);
  await page.evaluate(() => { const s=(window as any).fixture; s.visible=true; s.callbacks[s.listeners['main-visible']]({payload:true}); });
  await expect.poll(() => page.evaluate(() => (window as any).fixture.calls.filter((c: any) => ['accounts','usage','connection_status'].includes(c.command)).length)).toBeGreaterThan(count);
  await page.getByRole('button',{name:'重置额度',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'使用重置卡'})).toBeVisible();
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command==='reset_credits').length)).toBe(2);
});

test('failed reset queries show unknown and retry without consuming a card', async ({ page }) => {
  await fixture(page, ['reset_credits']);
  await expect(page.getByText(/重置卡查询失败；数量未知/)).toBeVisible();
  await expect(page.getByRole('button', { name:'当前首选账号', exact:true })).toBeEnabled();
  await page.evaluate(() => { (window as any).fixture.failures=[]; });
  await page.getByRole('button',{name:'刷新全部额度',exact:true}).click();
  await expect(page.getByText('可用重置卡 1 张',{exact:true})).toBeVisible();
  expect(await page.evaluate(()=>(window as any).fixture.calls.some((c:any)=>c.command==='consume_reset'))).toBe(false);
});

test('slow card reads do not block accounts or repeat with local polling', async ({ page }) => {
  await fixture(page, [], true, true);
  await expect(page.getByText('正在查询重置卡…',{exact:true})).toBeVisible();
  await expect(page.getByRole('button',{name:'当前首选账号',exact:true})).toBeEnabled();
  await page.waitForTimeout(3300);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='reset_credits').length)).toBe(1);
});

test('enabling and storage failures are reported independently', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { const s=(window as any).fixture;s.mode='enabling';s.service.storageWarning='fixture statistics were not saved'; });
  await expect(page.locator('.local-tag')).toHaveText('正在启动账号池',{timeout:8000});
  await expect(page.getByRole('button',{name:'正在启动…',exact:true})).toBeDisabled();
  await page.getByRole('button', { name: /通知中心/ }).click();
  await expect(page.getByRole('region', { name: '通知列表' })).toContainText('请求统计保存不完整');
});

test('unified inbox stays still without unread messages and detail has only OKAY', async ({ page }) => {
  test.setTimeout(60_000);
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.addInitScript(() => localStorage.setItem('pool-theme', 'dark'));
  await fixture(page);
  const trigger = page.getByRole('button', { name: /通知中心/ });
  const animation = () => page.locator('.notification-star i').evaluateAll(nodes => nodes.map(node => getComputedStyle(node).animationName));
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，0 条未读');
  expect(await animation()).toEqual(Array(5).fill('none'));
  await trigger.click();
  await expect(page.getByText('暂无通知', { exact: true })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(trigger).toBeFocused();
  await page.evaluate(() => { const s = (window as any).fixture; s.storageWarning = 'fixture statistics were not saved'; s.service.storageWarning = s.storageWarning; s.callbacks[s.listeners['main-visible']]({ payload: true }); });
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，1 条未读');
  expect(await animation()).toEqual(Array(5).fill('notification-rotate'));
  await expect(page.locator('main > .notice')).toHaveCount(0);
  await trigger.click();
  await expect(page.getByRole('region', { name: '通知列表' })).toContainText('请求统计保存不完整');
  await page.screenshot({ path: 'test-results/notifications-list-dark.png' });
  await page.getByRole('button', { name: /请求统计保存不完整/ }).click();
  const detail = page.getByRole('dialog', { name: '通知详情' });
  await expect(detail).toContainText('fixture statistics were not saved');
  await expect(detail.getByRole('button')).toHaveCount(1);
  await expect(detail.getByRole('button', { name: 'OKAY', exact: true })).toBeVisible();
  await page.screenshot({ path: 'test-results/notifications-detail-dark.png' });
  await detail.getByRole('button', { name: 'OKAY' }).click();
  await expect(trigger).toBeFocused();
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，0 条未读');
  expect(await animation()).toEqual(Array(5).fill('none'));
  await trigger.click();
  await expect(page.locator('.notification-row')).toHaveCount(1);
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('button', { name: '账号控制台', exact: true }).click();
  await expect(page.getByRole('region', { name: '通知列表' })).toHaveCount(0);
  await page.getByRole('switch').focus();
  await page.getByRole('switch').press('Space');
  await trigger.click();
  await page.getByRole('button', { name: /请求统计保存不完整/ }).click();
  await page.screenshot({ path: 'test-results/notifications-detail-light.png' });
  await page.keyboard.press('Escape');
  expect(await page.evaluate(() => (window as any).fixture.calls.some((c: any) => ['config_apply', 'close_pool', 'consume_reset', 'remove_account'].includes(c.command)))).toBe(false);
});

test('request notifications work off the logs page and collapse only the same conversation error', async ({ page }) => {
  await fixture(page);
  const trigger = page.getByRole('button', { name: /通知中心/ });
  await page.evaluate(() => {
    const s = (window as any).fixture;
    const start = Math.ceil(Date.now() / 1000); s.logStart = start;
    const log = (id: string, conversationKey: string, status: string, at: number) => ({ id, conversationKey, status, at: start + at, operation: 'responses', account: 'fixture-a', elapsedMs: 4000, advice: '上游未返回预期的事件流，可能存在网络拦截或协议变化' });
    s.logs = [log('first', 'thread:a', 'unexpected_content_type', 1000), log('repeat', 'thread:a', 'unexpected_content_type', 1010), log('other-conversation', 'thread:b', 'unexpected_content_type', 1011), log('other-error', 'thread:a', 'stream_interrupted', 1012)];
    s.callbacks[s.listeners['main-visible']]({ payload: true });
  });
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，3 条未读');
  await trigger.click();
  await expect(page.locator('.notification-row')).toHaveCount(3);
  await expect(page.getByText('已合并 2 次', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: '全部已读', exact: true }).click();
  await page.keyboard.press('Escape');
  await page.evaluate(() => { const s = (window as any).fixture; s.logs.push({ ...s.logs[0], id: 'another-repeat', at: s.logStart + 1020 }); s.callbacks[s.listeners['main-visible']]({ payload: true }); });
  await trigger.click();
  await expect(page.getByText('已合并 3 次', { exact: false })).toBeVisible();
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，0 条未读');
  await page.keyboard.press('Escape');
  await page.evaluate(() => { const s = (window as any).fixture; s.logs.push({ ...s.logs[0], id: 'later-incident', at: s.logStart + 1621 }); s.callbacks[s.listeners['main-visible']]({ payload: true }); });
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，1 条未读');
  await trigger.click();
  await expect(page.locator('.notification-row')).toHaveCount(4);
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  await expect(page.locator('.log')).toHaveCount(6);
  await expect(trigger).toHaveAttribute('aria-label', '通知中心，1 条未读');
});


test('request log defaults to conversations and folds successful system diagnostics', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const s = (window as any).fixture;
    s.logs.push({ id: 'system-check', at: 1, account: null, operation: 'models', status: 'success', elapsedMs: 10, advice: '', diagnostic: { appVersion: '0.1.5', upstreamStatus: 200, responseType: 'json', failureStage: null, proxySource: 'windows_static_configured' } });
  });
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  await expect(page.locator('.log')).toHaveCount(1);
  await expect(page.getByText('模型目录检查', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('combobox', { name: '选择对话', exact: true })).toHaveCount(0);
  await expect(page.locator('.conversation-summary')).toHaveCount(0);
  await page.evaluate(() => { document.documentElement.dataset.theme='dark'; });
  await page.screenshot({path:'test-results/request-records-all-dark.png',animations:'disabled'});
  const result=page.getByRole('combobox', { name: '筛选请求结果', exact: true });
  await result.click();
  await page.screenshot({path:'test-results/request-records-result-dark.png',animations:'disabled'});
  await page.keyboard.press('Escape');
  await expect(result).toBeFocused();
  await page.evaluate(() => { document.documentElement.dataset.theme='light'; });
  await result.click();
  await page.screenshot({path:'test-results/request-records-result-light.png',animations:'disabled'});
  await page.keyboard.press('Escape');
  await page.getByRole('radio', { name: '2 系统检查', exact: true }).check();
  await expect(page.getByRole('group', { name: '对话记录范围', exact: true })).toHaveCount(0);
  await expect(result).toBeVisible();
  await expect(page.locator('.log')).toHaveCount(1);
  await expect(page.locator('.log').getByText('模型目录检查', { exact: true })).toBeVisible();
  await expect(page.locator('.log .badge.warning')).toHaveCount(0);
  await expect(page.locator('.log-details .subtle').first()).not.toBeVisible();
  await page.getByText('诊断详情', { exact: true }).click();
  await expect(page.locator('.log-details .subtle').first()).toContainText('200');
  await expect(page.getByRole('button', { name: /通知中心/ })).toHaveAttribute('aria-label', '通知中心，0 条未读');
});

test('active request cancellation targets one request and waits for confirmation', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const s = (window as any).fixture;
    s.service.requests = [
      { id: 'request-owned', operation: 'responses', account: 'fixture-a', conversationKey: 'thread:fixture-hash',
        phase: 'headers', elapsedMs: 42000, idleMs: 40000 },
      { id: 'request-queued', operation: 'responses', account: 'fixture-a', conversationKey: null,
        phase: 'queued', elapsedMs: 12000, idleMs: 12000 },
    ];
  });
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  const panel = page.getByRole('region', { name: '当前请求', exact: true });
  await expect(panel).toContainText('处理中 1 · 排队 1');
  await expect(panel).toContainText('等待上游响应');
  await expect(panel).toContainText('等待空闲名额');
  await panel.getByRole('button', { name: '取消请求', exact: true }).first().click();
  const dialog = page.getByRole('dialog', { name: '取消请求', exact: true });
  await expect(dialog).toContainText('request-owned');
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'cancel_request').length)).toBe(0);
  await dialog.getByRole('button', { name: '确认取消并释放', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(panel).not.toContainText('request-owned');
  await expect(panel).toContainText('request-queued');
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'cancel_request'))).toEqual([
    { command: 'cancel_request', args: { id: 'request-owned' } },
  ]);
  await panel.screenshot({ path: 'test-results/active-requests.png' });
});

test('cooldown waits count as queued and can be cancelled before sending', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const state = (window as any).fixture;
    state.service.requests = [{ id: 'request-cooldown', operation: 'responses', account: 'fixture-a',
      conversationKey: 'thread:fixture-hash', phase: 'cooldown', elapsedMs: 8000, idleMs: 8000 }];
    state.callbacks[state.listeners['main-visible']]({ payload: true });
  });
  const panel = page.getByRole('region', { name: '当前请求', exact: true });
  await expect(panel).toContainText('处理中 0 · 排队 1');
  await expect(panel).toContainText('等待账号短期冷却结束，尚未发送（最多 60 秒）');
  await expect(panel).toContainText('已用 8 秒');
  await panel.getByRole('button', { name: '取消请求', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '取消请求', exact: true });
  await expect(dialog).toContainText('request-cooldown');
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'cancel_request').length)).toBe(0);
  await dialog.getByRole('button', { name: '确认取消并释放', exact: true }).click();
  await expect(panel).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).fixture.calls.filter((c: any) => c.command === 'cancel_request'))).toEqual([
    { command: 'cancel_request', args: { id: 'request-cooldown' } },
  ]);
});

test('response diagnostics show detected formats and preserve header declarations and unknown history', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const s = (window as any).fixture;
    s.logs = [
      ['sse-missing', 'responses', 'success', 200, 'missing', 'sse'],
      ['sse-mislabeled', 'responses', 'stream_interrupted', 200, 'json', 'sse'],
      ['json-missing', 'compact', 'success', 200, 'missing', 'json'],
      ['html', 'responses', 'unexpected_content_type', 200, 'html', undefined],
      ['no-response', 'responses', 'network_error', null, null, undefined],
      ['legacy-missing', 'responses', 'success', 200, 'missing', undefined],
    ].map(([id, operation, status, upstreamStatus, responseType, detectedResponseType]) => ({
      id, operation, status, at: 1, account: 'fixture-a', elapsedMs: 100, advice: '',
      diagnostic: { appVersion: '0.1.19', upstreamStatus, responseType, detectedResponseType,
        failureStage: null, proxySource: 'windows_static_configured' },
    }));
  });
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  const records = page.locator('.log');
  await expect(records).toHaveCount(6);
  const expected = [
    ['SSE 事件流（内容已识别）', '未提供 Content-Type'],
    ['SSE 事件流（内容已识别）', 'JSON'],
    ['JSON（内容已识别）', '未提供 Content-Type'],
    ['HTML 网页（响应头声明）', 'HTML 网页'],
    ['未收到响应', '未收到'],
    ['未识别', '未提供 Content-Type'],
  ];
  for (const [index, [detected, declared]] of expected.entries()) {
    const record = records.nth(index);
    await record.getByText('诊断详情', { exact: true }).click();
    await expect(record.locator('dt').filter({ hasText: /^响应类型$/ }).locator('+ dd')).toHaveText(detected);
    await expect(record.locator('dt').filter({ hasText: /^响应头声明$/ }).locator('+ dd')).toHaveText(declared);
  }
});

test('request diagnostics separate local rejection from HTTP 200 failure events', async ({ page }) => {
  await fixture(page);
  await page.evaluate(()=>{
    (window as any).fixture.logs=[
      {id:'local-rejected',status:'ingress_busy',at:1,account:null,operation:'responses',elapsedMs:10,advice:'本机等待队列已满，未发送至上游',diagnostic:{appVersion:'1.0.0',upstreamStatus:null,responseType:null,failureStage:'ingress_queue',proxySource:'unknown',execution:{localHttpStatus:503,upstreamAttempted:false,queueMs:null},details:null}},
      {id:'upstream-event',status:'response_failed',at:1,account:'fixture-a',operation:'responses',elapsedMs:100,advice:'上游返回失败事件',diagnostic:{appVersion:'1.0.0',upstreamStatus:200,responseType:'sse',detectedResponseType:'sse',failureStage:'upstream_rejected',proxySource:'windows_static_configured',execution:{localHttpStatus:200,upstreamAttempted:true,queueMs:1},details:{terminalEvent:'failed',upstreamFailure:'rate_limit'}}},
    ];
  });
  await page.getByRole('button',{name:'请求日志',exact:true}).click();
  const records=page.locator('.log');await expect(records).toHaveCount(2);
  for(let i=0;i<2;i++)await records.nth(i).getByText('诊断详情',{exact:true}).click();
  const field=(index:number,name:string)=>records.nth(index).locator('dt').filter({hasText:new RegExp(`^${name}$`)}).locator('+ dd');
  await expect(field(0,'失败阶段')).toHaveText('入口排队');await expect(field(0,'本机 HTTP')).toHaveText('503');
  await expect(field(0,'上游 HTTP')).toHaveText('未收到');await expect(field(0,'上游尝试')).toHaveText('未发送');
  await expect(field(1,'上游 HTTP')).toHaveText('200');await expect(field(1,'终止事件')).toHaveText('失败');
  await expect(field(1,'上游失败原因')).toHaveText('限流或额度限制');
});

test('network diagnostics distinguish transport failures and leave old causes unknown', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    const s = (window as any).fixture;
    s.logs = [
      ['stream-ended', 'stream_interrupted', 'stream_ended', 200, 'missing', 'response_read'],
      ['refused', 'network_error', 'connection_refused', null, null, 'connect'],
      ['legacy', 'stream_interrupted', undefined, 200, 'missing', 'response_read'],
    ].map(([id,status,networkErrorKind,upstreamStatus,responseType,failureStage]) => ({
      id,status,at:1,account:'fixture-a',operation:'responses',elapsedMs:100,advice:'',
      diagnostic:{appVersion:'0.1.16',upstreamStatus,responseType,failureStage,networkErrorKind,proxySource:'windows_static_configured'},
    }));
  });
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  await expect(page.locator('.log')).toHaveCount(3);
  const records = page.locator('.log');
  for (let index = 0; index < 3; index++) {
    const record = records.nth(index);
    await record.getByText('诊断详情', { exact: true }).click();
    await expect(record.getByText('网络错误分类', { exact: true })).toBeVisible();
  }
  await expect(records.nth(0)).toContainText('响应流提前结束，未收到完成事件');
  await expect(records.nth(1)).toContainText('连接被拒绝');
  const legacyKind = records.nth(2).locator('dt').filter({hasText:'网络错误分类'}).locator('+ dd');
  await expect(legacyKind).toHaveText('未提供');
  await expect(records.nth(2)).not.toContainText('连接被拒绝');
  await expect(records.nth(2)).not.toContainText('响应流提前结束，未收到完成事件');
});


test('completed close can immediately start again with one click', async ({page})=>{
  await fixture(page);
  await page.getByRole('button',{name:'关闭账号池',exact:true}).click();
  await page.evaluate(()=>{
    const s=(window as any).fixture;s.mode='off';s.operation=null;s.phase=null;s.error=null;s.applied=false;s.reloadRequired=null;s.service.running=false;s.service.draining=false;
    s.callbacks[s.listeners['close-complete']]({payload:null});
  });
  await expect(page.locator('.local-tag')).toHaveText('账号池已关闭');
  await page.getByRole('button',{name:'启动账号池',exact:true}).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='start_pool').length)).toBe(1);
  await page.evaluate(()=>{ const s=(window as any).fixture;s.mode='on';s.applied=true;s.phase=null;s.operation=null;s.service.running=true;s.callbacks[s.listeners['start-complete']]({payload:null}); });
  await expect(page.locator('.local-tag')).toHaveText('账号池已启动');
});

test('small work area keeps confirmation actions reachable and widget failures visible once', async ({ page }) => {
  await page.setViewportSize({ width: 683, height: 364 });
  await fixture(page);
  await page.getByRole('button', { name: '移除账号', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '确认操作' });
  await expect(dialog).toBeVisible();
  const cancel = dialog.getByRole('button', { name: '取消', exact: true });
  await cancel.scrollIntoViewIfNeeded();
  const box = await cancel.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.y + box!.height).toBeLessThanOrEqual(364);
  await cancel.click();
  await expect(dialog).toHaveCount(0);
  await page.evaluate(() => {
    const s = (window as any).fixture;
    const callback = s.callbacks[s.listeners['desktop-warning']];
    callback({ payload: '挂件与 Codex 的 DPI 模式不一致，已隐藏挂件' });
    callback({ payload: '挂件与 Codex 的 DPI 模式不一致，已隐藏挂件' });
  });
  await page.getByRole('button', { name: /通知中心/ }).click();
  await expect(page.locator('.notification-row').filter({ hasText: 'Codex 挂件不可用' })).toHaveCount(1);
});


test('launch animation only completes after backend success and focus or failure never reports success',async({page})=>{
  await fixture(page,[],false);
  if (!await page.getByRole('switch',{name:'黑夜模式'}).isChecked()) await page.locator('.theme-switch').click();
  const button=page.locator('.fusion-launch');
  await page.screenshot({path:'test-results/console-dark.png',animations:'disabled'});
  await button.focus();
  await expect(button).not.toHaveClass(/is-complete/);
  expect(await page.evaluate(()=>(window as any).fixture.calls.filter((c:any)=>c.command==='start_pool').length)).toBe(0);
  await button.click(); await expect(button).toHaveClass(/is-working/);
  await page.evaluate(()=>{Object.assign((window as any).fixture,{mode:'off',phase:'failed',operation:'start',error:'测试启动失败'});});
  await expect(button).toHaveAttribute('aria-label','重试启动');
  await expect(button).not.toHaveClass(/is-complete/);
  await button.click(); await expect(button).toHaveClass(/is-working/);
  await page.evaluate(()=>{Object.assign((window as any).fixture,{mode:'on',phase:null,operation:'start',error:null});});
  await expect(button).toHaveAttribute('aria-label','关闭账号池');
  await expect(button).toHaveClass(/is-complete/);
  await page.screenshot({path:'test-results/launch-completed.png',animations:'disabled'});
});
