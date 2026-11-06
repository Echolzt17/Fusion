import { test, expect, type Page } from '@playwright/test';
import { quotaIsStale, quotaUpdatedLabel } from '../../src/quota';
import { subscriptionRemaining } from '../../src/SubscriptionCountdown';

async function fixture(page: Page) {
  await page.addInitScript(() => {
    const at = Math.floor(Date.now() / 1000);
    const tokens = { inputTokens:15483218,outputTokens:108460,totalTokens:15591678,cachedTokens:6010000,reasoningTokens:62500 };
    const quota = { updatedAt:at-180, rateLimit:{primary_window:{used_percent:17,limit_window_seconds:604800,reset_at:at+86400},secondary_window:{used_percent:32,limit_window_seconds:3600,reset_at:at+900}} };
    const accounts = Array.from({length:3},(_,i)=>({id:'fixture-'+i,email:i?'long-account-name-'+i+'@example.test':'fixture@example.test',available:true,state:'ready',cooldownUntil:0,planLabel:i?'Plus':'Pro 200',quota:{...quota,subscription:i===1?null:{expiresAt:at+(i===0?12*86400+3*3600+24*60+52:-3600),checkedAt:at-3600}},quotaError:null,quotaRefreshing:false}));
    const logs = Array.from({length:3},(_,i)=>({id:'request-'+i,at,startedAt:at*1000,account:'fixture-0',conversationKey:'thread:123456789abc',operation:'responses',status:'success',requestedModel:i?'very-long-model-name-for-layout-check-'+('x'.repeat(48)):'gpt-6.1-sol',elapsedMs:21685,usage:tokens}));
    const state = {accounts,logs,callbacks:{} as Record<number,(event:unknown)=>void>,listeners:{} as Record<string,number>,calls:[] as string[]};
    Object.assign(window,{typeFixture:state,__TAURI_INTERNALS__:{
      transformCallback:(callback:(event:unknown)=>void)=>{const id=Object.keys(state.callbacks).length+1;state.callbacks[id]=callback;return id;},
      invoke:async(command:string,args:Record<string,unknown>={})=>{
        state.calls.push(command);
        if(command==='plugin:event|listen'){state.listeners[String(args.event)]=Number(args.handler);return 1;}
        switch(command){
          case 'accounts':return state.accounts;
          case 'main_visible':return true;
          case 'uninstall_requested':return false;
          case 'settings':return {port:18765,concurrency:2,fixedAccount:null,selectionPolicy:'auto'};
          case 'service_status':return {running:true,activeRequests:0,draining:false,baseUrl:'http://127.0.0.1:18765/v1'};
          case 'connection_status':return {mode:'on',configured:true,recoveryPending:true,operation:null,phase:null};
          case 'scheduling_status':return {policy:'auto',currentAccountId:'fixture-0',preferredAccountId:null};
          case 'login_status':return {phase:'idle',message:''};
          case 'image_compatibility':return {configurationReady:true};
          case 'logs':return state.logs;
          case 'usage':return [{account:'fixture-0',requests:12,measuredRequests:12,tokens,days:[]}];
          case 'usage_dashboard':return {tokens,measured:true,days:[],hours:[],today:'',timezone:'UTC+08:00',updatedAt:at*1000,scanning:false,historyPartial:false,activityPartial:false,error:null};
          case 'reset_credits':return {available_count:2,credits:[]};
          case 'conversation_usage':return {conversations:[],catalog:{requests:{branches:[{conversationKey:'thread:123456789abc',operation:'responses',requests:3}],accounts:['fixture-0'],models:['gpt-6.1-sol'],unknownModel:false},system:{branches:[],accounts:[],models:[],unknownModel:false}}};
          case 'request_history':return {items:state.logs,nextCursor:null};
          case 'refresh_quota':return;
          case 'plugin:event|unlisten':return;
          default:throw new Error('Unexpected fixture IPC: '+command);
        }
      },
    }});
  });
  await page.goto('/');
  await expect(page.getByRole('heading',{name:'fixture@example.test',exact:true})).toBeVisible();
  await page.evaluate(()=>document.fonts.ready);
}

test('quota age labels use the ten-minute boundary without inventing a fresh timestamp',()=>{
  expect(quotaIsStale(1000,1600_000)).toBe(false);
  expect(quotaIsStale(1000,1601_000)).toBe(true);
  expect(quotaIsStale(1001,1000_000)).toBe(true);
  expect(quotaUpdatedLabel(1000,1180_000)).toBe('3 分钟前更新');
  expect(quotaUpdatedLabel(1000,1001_000)).toBe('刚刚更新');
});

test('membership countdown keeps days, time and the expiry boundary separate from quota resets',()=>{
  expect(subscriptionRemaining(12*86400+3*3600+24*60+52,0)).toBe('剩余 12 天 03:24:52');
  expect(subscriptionRemaining(1,1)).toBe('剩余 0 天 00:00:01');
  expect(subscriptionRemaining(1,1000)).toBe('快照已到期，待确认');
  expect(subscriptionRemaining(1,2000)).toBe('快照已到期，待确认');
});

test('account cards tick locally and distinguish missing, past and free membership dates',async({page})=>{
  await page.clock.install({time:new Date('2026-11-03T04:00:00Z')});
  await page.clock.pauseAt(new Date('2026-11-03T04:00:00Z'));
  await fixture(page);
  const expiry=page.locator('.subscription-expiry');
  await expect(expiry).toHaveCount(3);
  await expect(expiry.nth(0)).toContainText('剩余 12 天 03:24:52');
  await expect(expiry.nth(0)).toContainText('授权快照');
  await expect(expiry.nth(0).locator('time')).toHaveAttribute('datetime','2026-11-15T07:24:52.000Z');
  await expect(expiry.nth(1)).toContainText('到期时间未知');
  await expect(expiry.nth(2)).toContainText('快照已到期，待确认');
  await expect(page.locator('.account').nth(2).locator('.badge')).toHaveText('可用');
  await page.clock.fastForward(2000);
  await expect(expiry.nth(0)).toContainText('剩余 12 天 03:24:50');
  expect(await page.evaluate(()=>(window as any).typeFixture.calls.filter((c:string)=>c==='refresh_quota').length)).toBe(0);
  await page.evaluate(()=>{const s=(window as any).typeFixture;s.accounts=s.accounts.map((a:any,i:number)=>i===1?{...a,planLabel:'Free',quota:{...a.quota,planType:'free'}}:a);});
  await page.clock.fastForward(3000);
  await expect(expiry.nth(1)).toContainText('免费套餐，无需倒计时');
});

test('local fonts, clear quota states and manual refresh remain independent of local polling',async({page})=>{
  await fixture(page);
  expect(await page.evaluate(()=>document.fonts.check('400 14px Inter') && document.fonts.check('400 14px "Source Han Sans CN"','额度更新'))).toBe(true);
  const fonts=await page.evaluate(()=>performance.getEntriesByType('resource').map(r=>new URL(r.name).pathname).filter(p=>p.endsWith('.woff2')));
  expect(fonts).toContain('/fonts/InterVariable-4.1.woff2');
  expect(fonts).toContain('/fonts/SourceHanSansCN-2.005.woff2');
  await expect(page.locator('.quota-update').first()).toContainText('3 分钟前更新');
  await expect(page.locator('.quota-update time').first()).toHaveAttribute('title',/^查询时间：/);
  await expect(page.locator('.quota-refresh-note')).toContainText('每 10 分钟自动更新');
  await expect(page.locator('.usage-details small').first()).toHaveCSS('font-size','12px');
  await page.evaluate(()=>{const s=(window as any).typeFixture;s.accounts[0].quotaRefreshing=true;});
  await expect(page.locator('.quota-refreshing')).toContainText('更新中');
  await page.evaluate(()=>{const s=(window as any).typeFixture;s.accounts[0].quotaRefreshing=false;s.accounts[0].quotaError='额度查询网络失败';});
  await expect(page.locator('.quota-meta').first()).toContainText('保留上次结果');
  await expect(page.locator('.quota-update time').first()).toContainText('3 分钟前更新 · 上次数据');
  expect(await page.evaluate(()=>(window as any).typeFixture.calls.filter((c:string)=>c==='refresh_quota').length)).toBe(0);
  await page.getByRole('button',{name:'刷新全部额度',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).typeFixture.calls.filter((c:string)=>c==='refresh_quota').length)).toBe(3);
});

test('main typography fits light, dark, narrow and scaled layouts with readable records',async({browser})=>{
  for(const scale of [1,1.25,1.5,2]){
    const context=await browser.newContext({viewport:{width:Math.floor(1920/scale),height:Math.floor(1080/scale)},deviceScaleFactor:scale});
    const page=await context.newPage();await fixture(page);
    await expect(page.locator('.usage-details b').first()).toHaveCSS('font-size','18px');
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    if(scale===1){
      await page.screenshot({path:'test-results/accounts-typography-light.png',fullPage:true,animations:'disabled'});
      await page.getByRole('switch',{name:'黑夜模式'}).check({force:true});
      await page.screenshot({path:'test-results/accounts-typography-dark.png',fullPage:true,animations:'disabled'});
    }
    await page.getByRole('button',{name:'请求日志',exact:true}).click();
    await expect(page.locator('.record-list>.log')).toHaveCount(3);
    await expect(page.locator('.record-list>.log').first().locator('.log-top>strong')).toHaveCSS('font-size','14px');
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    if(scale===1){
      const columns=await page.locator('.request-filters').evaluate(e=>getComputedStyle(e).gridTemplateColumns.split(' ').length);
      expect(columns).toBe(4);
      await page.screenshot({path:'test-results/records-typography-dark.png',fullPage:true,animations:'disabled'});
    }
    await page.locator('.log-details').first().locator('summary').click();
    await expect(page.locator('.log-details').first()).toHaveAttribute('open','');
    if(scale===2){await page.evaluate(()=>scrollTo(0,0));await page.screenshot({path:'test-results/records-typography-200.png',fullPage:true,animations:'disabled'});}
    await context.close();
  }
});
