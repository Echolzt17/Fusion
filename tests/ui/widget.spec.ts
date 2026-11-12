import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, widget = true) {
  await page.addInitScript(()=>{
    const tokens={inputTokens:10000,outputTokens:2400,totalTokens:12400,cachedTokens:null,reasoningTokens:500};
    const accounts=['a','b'].map((id,i)=>({id,email:'fixture-'+id+'@example.test',state:'ready',available:true,planLabel:'Plus',cooldownUntil:0,quota:null,quotaError:null}));
    const logs=Array.from({length:77},(_,i)=>({id:'request-'+i,at:100-i,startedAt:(100-i)*1000,account:i<61?'a':'b',conversationKey:i<61?'thread:aaaaaaaaaaaa':i<76?'thread:bbbbbbbbbbbb':null,operation:i<57?'responses':i===57?'compact':i===58?'image_generate':i<61?'image_edit':'responses',requestedModel:'test-requested',responseModel:i<76?'test-returned':null,status:i<76?'success':'cancelled',elapsedMs:180,advice:'',usage:i<76?tokens:null,diagnostic:{appVersion:'0.1.6',upstreamStatus:200,responseType:'sse',failureStage:null,proxySource:'windows_static_configured'}}));
    logs.push({...logs[0],id:'check',operation:'models',conversationKey:null});
    const conversations=['aaaaaaaaaaaa','bbbbbbbbbbbb','pruned'].map((id,i)=>({conversationKey:'thread:'+id,requests:i?15:61,measuredRequests:i?15:61,tokens,firstObservedAt:1000,lastStartedAt:i?2000:3000,lastAccount:i?'b':'a',historyPartial:i===2}));
    const update={operationActive:false,startupPromptPending:false,currentVersion:'0.1.31',phase:'current',version:null as string|null,notes:null as string|null,downloaded:0,total:null as number|null,message:null as string|null,installing:false};
    const state={control:{instance:"fixture-widget",revision:0,expanded:false,ready:true},sequence:0,gesture:0,update,calls:[] as {command:string;args:any}[],logs,conversations,on:true,error:false,failHistory:false,quotaAccounts:2,poolRemaining:148 as number|null,remaining:68 as number|null,tokens:942400 as number|null,accountSource:'recent_request',plan:'Plus',quotaState:'valid',recent:'thread:aaaaaaaaaaaa' as string|null,active:0,listeners:{} as Record<string,number>,callbacks:{} as Record<number,(e:any)=>void>};
    const catalog=()=>{
      const scope=(system:boolean)=>{
        const items=logs.filter(l=>(l.operation==='models')===system);
        const branches:any[]=[];
        for(const l of items){let b=branches.find(b=>b.conversationKey===l.conversationKey && b.operation===l.operation);if(!b){b={conversationKey:l.conversationKey,operation:l.operation,requests:0};branches.push(b);}b.requests++;}
        return {branches,accounts:[...new Set(items.map(l=>l.account))],models:[...new Set(items.flatMap(l=>[l.requestedModel,l.responseModel]).filter(Boolean))],unknownModel:items.some(l=>!l.requestedModel&&!l.responseModel)};
      };return {requests:scope(false),system:scope(true)};
    };
    const index=()=>({conversations,catalog:catalog(),recentRequest:{id:'latest',conversationKey:state.recent,startedAt:5000,account:state.recent==='thread:bbbbbbbbbbbb'?'b':'a'}});
    Object.assign(window,{widgetFixture:state,__TAURI_INTERNALS__:{transformCallback:(fn:any)=>{const id=Object.keys(state.callbacks).length+1;state.callbacks[id]=fn;return id;},invoke:async(command:string,args:any={})=>{
      state.calls.push({command,args});
      if(command==='plugin:event|listen'){state.listeners[args.event]=args.handler;return 1;}
      if(command==='plugin:event|unlisten')return;
      if(command==='widget_snapshot'){
        if(state.error)throw new Error('fixture failure');
        const summarize=(items:any[])=>{
          const models:any[]=[];const operations:Record<string,number>={};
          for(const log of items){const name=log.responseModel||log.requestedModel||'';const requested=!log.responseModel&&!!log.requestedModel;let model=models.find(m=>m.name===name&&m.requested===requested);if(!model){model={name,requested,requests:0};models.push(model);}model.requests++;operations[log.operation]=(operations[log.operation]||0)+1;}
          return {requests:items.length,measuredRequests:items.filter(l=>l.usage).length,totalTokens:items.reduce((n,l)=>n+(l.usage?.totalTokens||0),0),success:items.filter(l=>l.status==='success').length,failed:items.filter(l=>l.status==='network_error').length,other:items.filter(l=>!['success','network_error'].includes(l.status)).length,unclassifiedRequests:0,historyPartial:false,models,operations};
        };
        const business=logs.filter(l=>l.operation!=='models');const all=summarize(business);
        const current=state.recent?summarize(business.filter(l=>l.conversationKey===state.recent)):null;
        return {control:{...state.control},on:state.on,remaining:state.remaining,totalRemaining:state.poolRemaining,quotaAccounts:state.quotaAccounts,totalAccounts:2,tokens:state.tokens,usagePartial:all.measuredRequests<all.requests,conversationKey:state.recent,currentAccountLabel:state.accountSource==='unassigned'?null:state.recent==='thread:bbbbbbbbbbbb'?'账号2':'账号1',currentAccountSource:state.accountSource,currentPlanLabel:state.plan,currentQuota:{updatedAt:1700000000,state:state.quotaState,primaryWindow:{remaining:state.remaining,resetAt:1700018000,windowSeconds:18000},secondaryWindow:{remaining:state.remaining===null?null:83,resetAt:1700604800,windowSeconds:604800}},details:args.details?{conversationKey:state.recent,current,all,activeCurrent:state.active,activeAll:state.active}:undefined};
      }
      if(command==='conversation_usage')return index();
      if(command==='request_history'){
        if(state.failHistory)throw new Error('history unavailable');
        const f=args.filter||{};
        const filtered=logs.filter(l=>(l.operation==='models')===!!args.system&&(!args.conversationKey||l.conversationKey===args.conversationKey)&&(!f.unknownConversation||!l.conversationKey)&&(!f.operations?.length||f.operations.includes(l.operation))&&(f.account==null||l.account===f.account)&&(f.model==null||l.requestedModel===f.model||l.responseModel===f.model)&&(args.result==='all'||(args.result==='success'?l.status==='success':l.status!=='success')));
        const offset=args.cursor?.row||0;return {items:filtered.slice(offset,offset+50),nextCursor:filtered.length>offset+50?{at:1,row:offset+50}:null};
      }
      if(command==='update_status')return structuredClone(state.update);
      if(command==='check_update'){state.update.phase='checking';return;}
      if(command==='dismiss_update_prompt'){if(state.update.installing)throw new Error('正在安全收尾');state.update.startupPromptPending=false;state.update.operationActive=false;state.update.phase=state.update.version?'available':'current';return;}
      if(command==='open_project_repository')return;
      if(command==='install_update'){state.update.operationActive=true;state.update.startupPromptPending=false;state.update.phase='downloading';return;}
      if(command==='open_update_repair')return;
      if(command==='accounts')return accounts;
      if(command==='api_client_status')return [];
      if(command==='usage')return [];
      if(command==='logs')return logs;
      if(command==='settings')return {port:18765,concurrency:2,fixedAccount:null,selectionPolicy:'auto'};
      if(command==='service_status')return {running:false,draining:false,activeRequests:0,baseUrl:'http://127.0.0.1:18765/v1'};
      if(command==='connection_status')return {mode:'off',configured:false,recoveryPending:false,reloadRequired:null,error:null};
      if(command==='login_status')return {phase:'idle',message:''};
      if(command==='scheduling_status')return {policy:'auto',currentAccountId:'a',preferredAccountId:null};
      if(command==='image_compatibility')return {client:{},desktopAcceptance:'pending'};
      if(command==='main_visible')return true;
      if(command==='uninstall_requested')return false;
      if(command==='reset_credits')return {available_count:0,credits:[]};
      if(command==='widget_ready')return {...state.control};
      if(command==='widget_set_expanded'){state.control={...state.control,revision:state.control.revision+1,expanded:args.expanded};return {...state.control};}
      if(['widget_drag','widget_open_console','widget_input_ack'].includes(command))return;
      throw new Error('unexpected IPC '+command);
    }}});
  });
  await page.setViewportSize(widget?{width:416,height:520}:{width:1180,height:850});
  await page.goto(widget?'/?view=widget':'/');
  if(widget) await expect(page.getByRole('button',{name:'展开或收起账号池详情'})).toBeVisible();
  else await page.getByRole('button',{name:'请求日志',exact:true}).click();
}
async function open(page:Page){await page.getByRole('button',{name:'展开或收起账号池详情'}).click();await expect(page.locator('.widget-panel')).toBeVisible();await expect(page.locator('.widget-request-total')).toBeVisible();}

test('DOM mouse operations preserve the editor (does not exercise Windows IME)',async({page})=>{
  await fixture(page);
  await page.evaluate(()=>{
    const editor=document.createElement('textarea');editor.id='isolated-editor';
    editor.style.cssText='position:fixed;left:0;top:0;width:1px;height:1px';
    document.body.append(editor);editor.focus();
  });
  const editor=page.locator('#isolated-editor');
  await open(page);
  await expect(editor).toBeFocused();
  await page.keyboard.type('before ');
  await page.getByRole('tab',{name:'全部对话',exact:true}).click();
  await expect(editor).toBeFocused();
  await page.keyboard.insertText('中文输入');
  await page.getByRole('button',{name:'展开或收起账号池详情'}).click();
  await expect(editor).toBeFocused();
  await page.keyboard.type(' after');
  await expect(editor).toHaveValue('before 中文输入 after');
});

test('native pointer actions operate the read-only panel without focusing the WebView DOM',async({page})=>{
  await fixture(page);
  await page.evaluate(()=>{
    const editor=document.createElement('textarea');editor.id='isolated-editor';
    editor.style.cssText='position:fixed;left:0;top:0;width:1px;height:1px';
    document.body.append(editor);editor.focus();
  });
  const pointer=async(locator:ReturnType<Page['locator']>,kind='click',delta=0)=>{
    const rect=await locator.boundingBox();expect(rect).not.toBeNull();
    await page.evaluate(({rect,kind,delta})=>{
      const f=(window as any).widgetFixture;
      const send=(kind:string)=>{if(kind==='down')f.gesture++;f.callbacks[f.listeners['widget-pointer']]({payload:{instance:f.control.instance,sequence:++f.sequence,gesture:f.gesture,kind,x:rect!.x+rect!.width/2,y:rect!.y+rect!.height/2,delta}});};
      if(kind==='click'){send('down');send('up');}else send(kind);
    },{rect,kind,delta});
  };
  const editor=page.locator('#isolated-editor');
  const toggle=page.getByRole('button',{name:'展开或收起账号池详情'});
  await pointer(toggle,'down');await pointer(toggle,'cancel');await pointer(toggle,'up');
  await expect(page.locator('.widget-panel')).not.toBeVisible();
  for(let round=0;round<12;round++){
    await pointer(toggle);await expect(page.locator('.widget-panel')).toBeVisible();
    await pointer(page.getByRole('tab',{name:'全部对话',exact:true}));
    await expect(page.getByRole('tab',{name:'全部对话',exact:true})).toHaveAttribute('aria-selected','true');
    await expect(editor).toBeFocused();
    await pointer(toggle);await expect(page.locator('.widget-panel')).not.toBeVisible();
    await expect(editor).toBeFocused();
  }
  await pointer(toggle);await expect(page.locator('.widget-panel')).toBeVisible();
  await page.locator('.widget-summary-body').evaluate(body=>{
    const extra=document.createElement('div');extra.style.height='1000px';body.append(extra);
  });
  await pointer(page.locator('.widget-summary-body'),'wheel',-120);
  await expect.poll(()=>page.locator('.widget-summary-body').evaluate(e=>e.scrollTop)).toBeGreaterThan(0);
  await expect(editor).toBeFocused();
  await pointer(page.getByRole('button',{name:'打开控制台'}));
  await expect.poll(()=>page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='widget_open_console').length)).toBe(1);
  await pointer(page.locator('.widget-grip'),'down');
  await expect.poll(()=>page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='widget_drag').length)).toBe(1);
});

test('old instances and late revisions cannot toggle or replay native actions', async ({page}) => {
  await fixture(page);
  const toggle = page.getByRole('button', {name:'展开或收起账号池详情'});
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded','true');
  await page.evaluate(() => {
    const f=(window as any).widgetFixture;
    const send=(payload:any)=>f.callbacks[f.listeners['widget-expanded']]({payload});
    send({instance:'destroyed-instance',revision:10000,expanded:false,ready:true});
    send({instance:f.control.instance,revision:0,expanded:false,ready:true});
  });
  await expect(toggle).toHaveAttribute('aria-expanded','true');
  const box=await toggle.boundingBox();
  await page.evaluate(box => {
    const f=(window as any).widgetFixture;
    const pointer=(payload:any)=>f.callbacks[f.listeners['widget-pointer']]({payload});
    const base={instance:f.control.instance,gesture:77,x:box!.x+20,y:box!.y+20,delta:0};
    pointer({...base,sequence:100,kind:'down'});pointer({...base,sequence:101,kind:'up'});
    pointer({...base,sequence:101,kind:'up'}); // repeated delivery must not replay a click
    pointer({...base,instance:'destroyed-instance',sequence:10000,kind:'down'});
    pointer({...base,instance:'destroyed-instance',sequence:10001,kind:'up'});
  },box);
  await expect(toggle).toHaveAttribute('aria-expanded','false');
  await expect.poll(()=>page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='widget_input_ack' && c.args.completed).length)).toBe(1);
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='widget_set_expanded').length)).toBe(2);
});

test('the actual 416 by 40 collapsed viewport never clips the capsule after opening and closing',async({browser})=>{
  for(const scale of [1,1.25,1.5,2]){
    const context=await browser.newContext({viewport:{width:416,height:40},deviceScaleFactor:scale});
    const page=await context.newPage();await fixture(page);
    await page.setViewportSize({width:416,height:40});
    await page.evaluate(()=>document.fonts.ready);
    const fullyVisible=async()=>{
      const boxes=await page.evaluate(()=>{
        const rect=(selector:string)=>{const r=document.querySelector(selector)!.getBoundingClientRect();return {top:r.top,bottom:r.bottom,height:r.height};};
        return {root:rect('.widget-root'),capsule:rect('.widget-capsule'),button:rect('.widget-capsule button'),height:innerHeight};
      });
      expect(boxes.root.height).toBe(40);
      for(const box of [boxes.capsule,boxes.button]){expect(box.top).toBeGreaterThanOrEqual(0);expect(box.bottom).toBeLessThanOrEqual(boxes.height);}
      expect(boxes.capsule.height).toBe(40);
      expect(await page.locator('.widget-capsule button').evaluate(e=>e.scrollWidth<=e.clientWidth)).toBe(true);
    };
    await fullyVisible();
    if(scale===1)await page.screenshot({path:'test-results/widget-collapsed-40px.png',animations:'disabled'});
    await page.getByRole('button',{name:'展开或收起账号池详情'}).click();
    await page.setViewportSize({width:416,height:520});
    await expect(page.locator('.widget-request-total')).toBeVisible();
    const footer=await page.locator('.widget-summary-footer').boundingBox();
    expect(footer!.y+footer!.height).toBeLessThanOrEqual(480);
    await page.keyboard.press('Escape');
    await page.setViewportSize({width:416,height:40});
    await expect(page.locator('.widget-panel')).toBeHidden();
    await fullyVisible();
    await context.close();
  }
});
async function navigate(page:Page){await page.getByRole('button',{name:/^分类导航/}).click();}
async function select(page:Page,name:string,option:string){await page.getByRole('combobox',{name,exact:true}).click();await page.getByRole('option',{name:option,exact:true}).click();await page.keyboard.press('Escape');}


test('capsule shows the actual pool account and plan; summary scope survives collapse',async({page})=>{
  await fixture(page);
  const capsule=page.locator('.widget-capsule');
  await expect(capsule).toHaveCSS('width','416px');await expect(capsule).toHaveCSS('height','40px');
  await expect(capsule).toContainText('池·账号1 Plus·余68%');await expect(capsule).toContainText('合计 148%');await expect(capsule).toContainText('Token 942.4k*');
  await expect(capsule).toContainText('账号1');
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='widget_snapshot').every((c:any)=>!c.args.details))).toBe(true);
  await open(page);
  await expect(page.getByRole('tab',{name:'本对话',exact:true})).toHaveAttribute('aria-selected','true');
  await expect(page.locator('.widget-request-total')).toContainText('61 次');
  await expect(page.locator('.widget-results')).toContainText('成功 61');
  await expect(page.locator('.widget-summary-footer')).toContainText('756,400');
  await expect(page.locator('.widget-summary-body')).toContainText('test-returned');
  await expect(page.locator('.widget-summary-body')).not.toContainText('test-requested');
  await expect(page.locator('.widget-summary-rows').last()).toContainText('图片编辑2 次');
  await expect(page.getByRole('region',{name:'额度来源'})).toContainText('最近实际请求账号');
  await expect(page.getByRole('region',{name:'额度来源'})).toContainText('5小时周期');
  await expect(page.getByRole('region',{name:'额度来源'})).toContainText('7天周期');
  await expect(page.getByRole('region',{name:'额度来源'})).toContainText('原生登录账号');
  await page.mouse.move(10,10);
  await page.screenshot({path:'test-results/widget-summary-current.png',animations:'disabled'});
  await expect(page.locator('.record-tree,.request-filters,.log-details,.log')).toHaveCount(0);
  await expect(page.getByRole('button',{name:'关闭账号池',exact:true})).toHaveCount(0);
  await page.getByRole('tab',{name:'全部对话',exact:true}).click();
  await expect(page.locator('.widget-request-total')).toContainText('77 次');
  await expect(page.locator('.widget-results')).toContainText('其他 1');
  await expect(page.locator('.widget-summary-footer')).toContainText('942,400');
  await page.keyboard.press('Escape');await expect(page.locator('.widget-panel')).toBeHidden();
  await page.waitForTimeout(3200);
  await open(page);
  await expect(page.getByRole('tab',{name:'全部对话',exact:true})).toHaveAttribute('aria-selected','true');
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.some((c:any)=>['request_history','refresh_quota','reset_credits','consume_reset','login','close_pool'].includes(c.command)))).toBe(false);
  await page.getByRole('button',{name:'打开控制台'}).click();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.find((c:any)=>c.command==='widget_open_console').args)).toEqual({instance:'fixture-widget'});
});

test('rapid toggles settle on the latest intent and closed panel cannot retain keyboard focus',async({page})=>{
  await fixture(page);
  const toggle=page.getByRole('button',{name:'展开或收起账号池详情'});
  await toggle.evaluate(button=>{for(let i=0;i<5;i++)(button as HTMLButtonElement).click();});
  await expect(toggle).toHaveAttribute('aria-expanded','true');
  await expect(page.locator('.widget-panel')).toBeVisible();
  await toggle.evaluate(button=>{for(let i=0;i<3;i++)(button as HTMLButtonElement).click();});
  await expect(toggle).toHaveAttribute('aria-expanded','false');
  await expect(page.locator('.widget-panel')).toHaveAttribute('inert','');
  await expect(page.locator('.widget-panel')).toBeHidden();
});

test('collapse and native cursor correction clear hover and focus even without pointerleave',async({page})=>{
  await fixture(page);
  const capsule=page.locator('.widget-capsule');
  const toggle=page.getByRole('button',{name:'展开或收起账号池详情'});
  const swept=()=>capsule.evaluate(e=>parseFloat(getComputedStyle(e,'::before').left));
  await toggle.hover();
  await expect.poll(swept).toBeGreaterThanOrEqual(0);
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded','true');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded','false');
  await page.setViewportSize({width:416,height:40});
  await expect(toggle).not.toBeFocused();
  await toggle.hover();
  await expect.poll(swept).toBeGreaterThanOrEqual(0);
  // Keep CSS :hover true, just as when a child resize loses WebView2's leave.
  await page.evaluate(()=>window.dispatchEvent(new Event('blur')));
  await expect.poll(swept).toBeLessThan(-300);
  await page.mouse.move(1,1);await toggle.hover();
  await expect.poll(swept).toBeGreaterThanOrEqual(0);
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.callbacks[s.listeners['widget-hovered']]({payload:false});});
  await expect.poll(swept).toBeLessThan(-300);
  await toggle.focus();await page.keyboard.press('Enter');
  await expect(toggle).toHaveAttribute('aria-expanded','true');
  await page.keyboard.press('Escape');
  await expect(toggle).toHaveAttribute('aria-expanded','false');
  await expect(toggle).not.toBeFocused();
  await expect.poll(swept).toBeLessThan(-300);
  await page.screenshot({path:'test-results/widget-collapsed-after-interaction.png',animations:'disabled'});
});

test('partial quota, unknown conversation, failed reads, drag and native collapse stay safe',async({page})=>{
  await fixture(page);await open(page);
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.quotaAccounts=1;s.poolRemaining=42;s.remaining=null;s.tokens=null;s.recent=null;});
  await expect(page.locator('.widget-capsule')).toContainText('·余—',{timeout:6000});
  await expect(page.locator('.widget-capsule')).toContainText('合计 42%*');
  await expect(page.locator('.widget-empty')).toContainText('未识别对话');
  await expect(page.locator('.widget-request-total')).toHaveCount(0);
  await page.getByRole('tab',{name:'全部对话',exact:true}).click();
  await expect(page.locator('.widget-request-total')).toContainText('77 次');
  await page.evaluate(()=>{(window as any).widgetFixture.error=true;});
  await expect(page.getByRole('alert')).toContainText('保留上次数据',{timeout:6000});
  await expect(page.locator('.widget-request-total')).toContainText('77 次');
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.control={...s.control,revision:s.control.revision+1,expanded:false};s.callbacks[s.listeners['widget-expanded']]({payload:{...s.control}});});
  await expect(page.locator('.widget-panel')).toBeHidden();
  await page.getByTitle('拖动挂件').dispatchEvent('mousedown',{button:0});
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.some((c:any)=>c.command==='widget_drag'))).toBe(true);
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.error=false;s.on=false;});
  await expect(page.locator('.widget-capsule')).toHaveCount(0,{timeout:6000});
});

test('account attribution, preference and stale quota stay explicit after account changes',async({page})=>{
  await fixture(page);await open(page);
  const source=page.getByRole('region',{name:'额度来源'});
  await expect(source).toContainText('最近实际请求账号');
  await expect(source).toContainText('账号1');
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.recent='thread:bbbbbbbbbbbb';s.plan='Pro 100';});
  await expect(page.locator('.widget-capsule')).toContainText('池·账号2 Pro 100·余68%',{timeout:6000});
  await expect(source).toContainText('账号2');
  await expect(source).toContainText('并发请求可能使用其他池账号');
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.accountSource='preferred';s.quotaState='stale';s.remaining=null;});
  await expect(page.locator('.widget-capsule')).toContainText('池·首选·账号2 Pro 100·余—',{timeout:6000});
  await expect(source).toContainText('首选账号 · 尚无业务请求');
  await expect(source).toContainText('已过期');
  await page.evaluate(()=>{const s=(window as any).widgetFixture;s.accountSource='unassigned';s.plan=null;s.quotaState='query_failed';});
  await expect(page.locator('.widget-capsule')).toContainText('池·待分配账号·余—',{timeout:6000});
  await expect(source).toContainText('最近请求 · 等待分配账号');
  await expect(source).toContainText('查询失败，额度未知');
  await expect(source).not.toContainText('账号2');
});

test('keyboard, animations, long model lists, themes and scaled small viewports',async({browser})=>{
  for(const scale of [1,1.25,1.5,2]){
    const context=await browser.newContext({viewport:{width:416,height:520},deviceScaleFactor:scale});
    const page=await context.newPage();await fixture(page);await open(page);
    await page.evaluate(()=>{const s=(window as any).widgetFixture;for(let i=0;i<24;i++)s.logs.push({...s.logs[0],id:'long-'+i,responseModel:'long-model-name-'+i+'x'.repeat(100)});});
    await expect(page.locator('.widget-request-total')).toContainText('85 次',{timeout:6000});
    await page.getByRole('tab',{name:'本对话',exact:true}).focus();await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('tab',{name:'全部对话',exact:true})).toBeFocused();
    await page.keyboard.press('Home');await expect(page.getByRole('tab',{name:'本对话',exact:true})).toBeFocused();
    expect(await page.locator('.widget-summary-body').evaluate(e=>e.scrollHeight>e.clientHeight)).toBe(true);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    await expect(page.locator('.widget-arrow')).toHaveCSS('transition-duration','0.3s, 0.3s, 0.3s');
    if(scale===1)await page.screenshot({path:'test-results/widget-summary-light.png',animations:'disabled'});
    await page.evaluate(()=>{localStorage.setItem('pool-theme','dark');dispatchEvent(new StorageEvent('storage',{key:'pool-theme'}));});
    await expect(page.locator('html')).toHaveAttribute('data-theme','dark');
    if(scale===2)await page.screenshot({path:'test-results/widget-summary-dark-200.png',animations:'disabled'});
    await page.setViewportSize({width:300,height:300});
    const footer=await page.getByRole('button',{name:'打开控制台'}).boundingBox();
    expect(footer!.x).toBeGreaterThanOrEqual(0);expect(footer!.x+footer!.width).toBeLessThanOrEqual(300);expect(footer!.y+footer!.height).toBeLessThanOrEqual(260);
    expect(await page.locator('.widget-capsule button').evaluate(e=>e.scrollWidth<=e.clientWidth)).toBe(true);
    await page.emulateMedia({reducedMotion:'reduce'});
    await expect(page.locator('.widget-arrow')).toHaveCSS('transition-duration','0s');
    await context.close();
  }
});

test('sidebar hover and keyboard emphasis use the theme and respect reduced motion',async({page})=>{
  await fixture(page,false);
  const accounts=page.getByRole('button',{name:'账号控制台',exact:true});const settings=page.getByRole('button',{name:'连接与设置',exact:true});
  await accounts.hover();await expect(settings).toHaveCSS('filter','blur(0.6px)');
  await expect.poll(()=>accounts.evaluate(e=>new DOMMatrix(getComputedStyle(e).transform).a)).toBeCloseTo(1.04,2);
  await accounts.click();await expect(accounts).toHaveAttribute('aria-current','page');
  await page.mouse.move(600,500);await page.keyboard.press('Tab');await settings.focus();
  await expect(accounts).toHaveCSS('filter','blur(0.6px)');
  await page.screenshot({path:'test-results/sidebar-focus-light.png',animations:'disabled'});
  await page.emulateMedia({reducedMotion:'reduce'});await expect(accounts).toHaveCSS('filter','none');await expect(settings).toHaveCSS('transform','none');
});

test('tree follows ownership, handles unknown and pruned conversations, and separates expand from select',async({page})=>{
  await fixture(page,false);await page.setViewportSize({width:800,height:650});await navigate(page);
  const tree=page.locator('.record-tree');
  await expect(page.getByRole('radio',{name:'1 全部对话'})).toBeChecked();
  await tree.getByRole('button',{name:'展开对话 T-aaaaaaaaaaaa',exact:true}).click();
  await expect(page.locator('.conversation-summary')).toHaveCount(0);
  await tree.getByRole('button',{name:'对话 T-aaaaaaaaaaaa 61',exact:true}).click();
  await expect(page.locator('.conversation-summary')).toContainText('长期累计 61');
  const conversation=tree.locator('.record-tree-item').filter({has:page.getByRole('button',{name:'对话 T-aaaaaaaaaaaa 61',exact:true})}).last();
  await conversation.getByRole('button',{name:'展开图片任务',exact:true}).click();
  await conversation.getByRole('button',{name:'图片任务 3',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(3);
  await conversation.getByRole('button',{name:'图片编辑 2',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(2);
  await expect(page.locator('.record-path')).toContainText('图片任务/图片编辑');
  await expect(page.locator('.request-identity strong')).toHaveCount(0);
  await page.locator('.record-path').getByRole('button',{name:'图片任务',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(3);
  await conversation.getByRole('button',{name:'上下文压缩 1',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(1);
  await tree.getByRole('button',{name:'未识别对话 1',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(1);
  await expect(page.locator('.log')).toContainText('用量未知');
  await tree.getByRole('button',{name:'对话 T-pruned 0',exact:true}).click();
  await expect(page.locator('.conversation-summary')).toContainText('累计仍保留，明细已清理');
  await page.keyboard.press('Escape');
  await expect(page.locator('.record-navigation')).toBeHidden();
  await expect(page.locator('.conversation-summary')).toBeVisible();
});

test('pinning and explicit follow do not change all-conversation paging',async({page})=>{
  await fixture(page,false);
  await page.getByRole('button',{name:'下一页',exact:true}).click();
  await page.evaluate(()=>{(window as any).widgetFixture.recent='thread:bbbbbbbbbbbb';});
  await page.waitForTimeout(3200);
  await expect(page.locator('.request-page-info')).toContainText('第 2 页');
  await page.locator('.record-tree').getByRole('button',{name:'对话 T-bbbbbbbbbbbb 15',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(15);
  await page.evaluate(()=>{(window as any).widgetFixture.recent='thread:aaaaaaaaaaaa';});
  await page.waitForTimeout(3200);
  await expect(page.locator('.conversation-summary')).toContainText('T-bbbbbbbbbbbb');
  await page.getByRole('button',{name:'跟随最近请求',exact:true}).click();
  await expect(page.locator('.conversation-summary')).toContainText('T-aaaaaaaaaaaa');
  await page.locator('.record-path').getByRole('button',{name:'对话 T-aaaaaaaaaaaa',exact:true}).click();
  await page.evaluate(()=>{(window as any).widgetFixture.recent='thread:bbbbbbbbbbbb';});
  await expect(page.locator('.conversation-summary')).toContainText('T-bbbbbbbbbbbb',{timeout:6000});
  await page.getByRole('button',{name:'固定查看',exact:true}).click();
  await expect(page.locator('.conversation-summary')).toContainText('固定查看');
});

test('combined filters, refresh failures, themes, keyboard, motion and viewport boundaries',async({page})=>{
  await fixture(page,false);
  await expect(page.locator('.log')).toHaveCount(50);
  await expect(page.getByRole('radio',{name:'1 全部对话'})).toBeChecked();
  await select(page,'筛选请求类型','图片编辑');
  await select(page,'筛选账号','fixture-a@example.test');
  await select(page,'筛选模型','test-requested');
  await expect(page.locator('.log')).toHaveCount(2);
  await expect(page.locator('.request-page-info')).toContainText('第 1 页');
  await page.locator('.log').first().getByText('用量信息',{exact:true}).click();
  await expect(page.locator('.log').first()).toContainText('缓存或推理明细未完整提供');
  await page.screenshot({path:'test-results/records-main-light.png',animations:'disabled'});
  await page.evaluate(()=>{document.documentElement.dataset.theme='dark';(window as any).widgetFixture.failHistory=true;});
  await expect(page.getByRole('alert')).toContainText('保留上次数据',{timeout:7000});
  await expect(page.locator('.log')).toHaveCount(2);
  await page.evaluate(()=>{(window as any).widgetFixture.failHistory=false;});
  await page.getByRole('button',{name:'清除筛选',exact:true}).click();
  await expect(page.locator('.log')).toHaveCount(50);
  await page.screenshot({path:'test-results/records-main-dark.png',animations:'disabled'});
  await expect(page.locator('.category-ball').first()).toHaveCSS('transition-duration','0.8s');
  await expect(page.locator('.tree-children-wrapper').first()).toHaveCSS('transition-duration','0.3s');
  await page.getByRole('radio',{name:'2 系统检查'}).check();
  await expect(page.locator('.log')).toHaveCount(1);
  await expect(page.locator('.log')).toContainText('模型目录检查');
  await expect(page.locator('.log').getByText('用量信息',{exact:true})).toHaveCount(0);
  await page.getByRole('radio',{name:'1 全部对话'}).focus();
  await page.keyboard.press('Space');
  await expect(page.locator('.log')).toHaveCount(50);
  await page.emulateMedia({reducedMotion:'reduce'});
  await expect(page.locator('.category-ball').first()).toHaveCSS('transition-duration','0s');
  await expect(page.locator('.tree-children-wrapper').first()).toHaveCSS('transition-duration','0s');
  await page.setViewportSize({width:800,height:650});
  await navigate(page);
  await page.screenshot({path:'test-results/records-main-narrow.png',animations:'disabled'});
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  const result=page.getByRole('combobox',{name:'筛选请求结果',exact:true});
  await result.focus();await page.keyboard.press('ArrowDown');await page.keyboard.press('End');await page.keyboard.press('Enter');
  await expect(result).toContainText('其他结果');
  const box=await page.locator('.record-select-popover').boundingBox();
  expect(box!.x).toBeGreaterThanOrEqual(0);expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x+box!.width).toBeLessThanOrEqual(800);expect(box!.y+box!.height).toBeLessThanOrEqual(650);
  await page.keyboard.press('Escape');await expect(result).toBeFocused();
  await expect(page.locator('.log')).toHaveCount(1);
});


test('startup update prompt defers without installing and all pages share the red update entry', async ({page})=>{
  await fixture(page,false);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  if (!await page.getByRole('switch',{name:'黑夜模式'}).isChecked()) await page.locator('.theme-switch').click();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'available',version:'0.1.32',notes:'# Fusion 0.1.32\n## 本次重点\n- <b>说明保持纯文本</b>',startupPromptPending:true});});
  const dialog=page.getByRole('dialog',{name:'Fusion 软件更新'});
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('<b>说明保持纯文本</b>',{exact:true})).toBeVisible();
  await expect(dialog.locator('b')).toHaveCount(0);
  await expect(dialog).toContainText('当前 v0.1.31'); await expect(dialog).toContainText('v0.1.32');
  await page.screenshot({path:'test-results/updates-dark.png',animations:'disabled'});
  await dialog.getByRole('button',{name:'稍后更新',exact:true}).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator('.update-press')).toHaveClass(/has-update/);
  await page.getByRole('button',{name:'连接与设置',exact:true}).click();
  await expect(page.locator('.update-panel,.pool-cube')).toHaveCount(0);
  await expect(page.getByRole('combobox',{name:'更新通道'})).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').length)).toBe(0);
  await page.getByRole('button',{name:'检查更新',exact:true}).click();
  await expect(dialog.getByRole('button',{name:'立即更新',exact:true})).toBeDisabled();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').length)).toBe(0);
  await page.evaluate(()=>{(window as any).widgetFixture.update.phase='available';});
  await dialog.getByRole('button',{name:'立即更新',exact:true}).click();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('button',{name:'更新进行中…'})).toBeDisabled();
  await expect(dialog.getByRole('button',{name:'稍后更新',exact:true})).toBeEnabled();
  await expect(dialog.locator('.update-caution')).toContainText('账号池已关闭时，更新不会关闭或重启 Codex');
  await expect(dialog.locator('.update-caution')).toContainText('更新后账号池保持关闭，点击“启动账号池”才打开 Codex');
  await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).widgetFixture.update.operationActive)).toBe(false);
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.find((c:any)=>c.command==='install_update').args.version)).toBe('0.1.32');
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').length)).toBe(1);
});

test('runtime discovery does not interrupt; failure retains candidate and repair action',async({page})=>{
  await fixture(page,false);
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'available',version:'0.1.32',notes:'新的更新说明'});});
  await expect(page.locator('.update-press')).toHaveClass(/has-update/);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('button',{name:'检查更新',exact:true}).click();
  await page.evaluate(()=>{(window as any).widgetFixture.update.phase='available';});
  await page.getByRole('button',{name:'立即更新',exact:true}).click();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'failed',operationActive:false,message:'更新包签名校验失败，未安装'});});
  await expect(page.getByText('更新包签名校验失败，未安装',{exact:true})).toBeVisible();
  await expect(page.locator('.update-press')).toHaveClass(/has-update/);
  await page.getByRole('button',{name:'打开修复目录',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='open_update_repair').length)).toBe(1);
  await page.getByRole('button',{name:'稍后更新',exact:true}).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('button',{name:/LEARN MORE/}).click();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='open_project_repository').length)).toBe(1);
});

test('manual ordinary entry only checks and latest version stays in the tooltip',async({page})=>{
  await fixture(page,false);
  await page.getByRole('button',{name:'检查更新',exact:true}).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'current',operationActive:false});});
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.update-press')).toHaveAttribute('title','当前已是最新版本');
  await expect(page.locator('.update-caption,.update-notice')).toHaveCount(0);
  await expect(page.locator('.update-press')).not.toHaveClass(/has-update/);
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').length)).toBe(0);
});

test('opening an automatic check joins discovery without installing or leaving an empty dialog',async({page})=>{
  await fixture(page,false);
  await page.evaluate(()=>{(window as any).widgetFixture.update.phase='checking';});
  await page.getByRole('button',{name:'正在检查更新',exact:true}).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.evaluate(()=>{(window as any).widgetFixture.update.phase='current';});
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.update-press')).toHaveAttribute('title','当前已是最新版本');
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>['install_update','check_update'].includes(c.command)).length)).toBe(0);
});

test('manual discovery waits for confirmation; repair progress can defer until safe installation',async({page})=>{
  await fixture(page,false);
  await page.getByRole('button',{name:'检查更新',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'Fusion 软件更新'});
  await expect(dialog.getByRole('button',{name:'稍后更新',exact:true})).toBeEnabled();
  await expect(dialog.getByRole('button',{name:'立即更新',exact:true})).toBeDisabled();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'available',version:'0.1.33',notes:'确认这个版本后才下载'});});
  await expect(dialog.getByRole('button',{name:'立即更新',exact:true})).toBeEnabled();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').length)).toBe(0);
  await dialog.getByRole('button',{name:'立即更新',exact:true}).click();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'preparing',downloaded:1048576,total:2097152});});
  await expect(dialog.getByRole('progressbar')).toHaveAttribute('value','50');
  await expect(dialog.getByRole('button',{name:'稍后更新',exact:true})).toBeEnabled();
  await dialog.getByRole('button',{name:'稍后更新',exact:true}).click();
  await expect(dialog).toHaveCount(0);
  await page.getByRole('button',{name:'检查更新',exact:true}).click();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'available'});});
  await dialog.getByRole('button',{name:'立即更新',exact:true}).click();
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'closing',installing:true});});
  await expect(dialog.getByRole('button',{name:'稍后更新',exact:true})).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeVisible();
  expect(await page.evaluate(()=>(window as any).widgetFixture.calls.filter((c:any)=>c.command==='install_update').map((c:any)=>c.args.version))).toEqual(['0.1.33','0.1.33']);
});

for (const scale of [1, 1.25, 1.5, 2]) {
  test.describe(`compact update entry at device scale ${scale}`, () => {
    test.use({deviceScaleFactor: scale});
    test('keeps the compact update entry left of notifications and the repository in the sidebar', async ({page}) => {
      await fixture(page, false);
      for (const width of [1180, 620]) {
        await page.setViewportSize({width, height: 760});
        for (const theme of ['light', 'dark']) {
          await page.emulateMedia({reducedMotion:'reduce'});
          const toggle = page.getByRole('switch', {name:'黑夜模式'});
          if (await toggle.isChecked() !== (theme === 'dark')) await page.locator('.theme-switch').click();
          const button = page.locator('.update-press');
          const hit = await button.boundingBox(), visual = await page.locator('.update-visual').boundingBox();
          const notifications = await page.locator('.notification-center').boundingBox();
          const repository = await page.locator('.repository-link').boundingBox();
          expect(hit!.width).toBeGreaterThanOrEqual(44); expect(hit!.height).toBeGreaterThanOrEqual(44);
          expect(visual!.width).toBe(30); expect(visual!.height).toBe(25);
          expect(hit!.x + hit!.width).toBeLessThanOrEqual(notifications!.x);
          expect(Math.abs(hit!.y + hit!.height / 2 - notifications!.y - notifications!.height / 2)).toBeLessThanOrEqual(1);
          await expect(page.locator('header .update-press')).toHaveCount(1);
          await expect(page.locator('aside .update-press')).toHaveCount(0);
          await expect(page.locator('aside .repository-link')).toHaveCount(1);
          await expect(page.locator('.update-caption,.update-notice')).toHaveCount(0);
          await button.focus(); await expect(button).toBeFocused();
          await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'failed',message:'检查更新失败，请重试'});});
          await expect(button).toHaveClass(/has-error/);
          await expect(button).toHaveAttribute('title','检查更新失败，请重试');
          expect(await page.locator('.repository-link').boundingBox()).toEqual(repository);
          if (scale === 1) await page.screenshot({path:`test-results/update-header-${theme}-${width}.png`, animations:'disabled'});
          await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'current',message:null});});
          await expect(button).not.toHaveClass(/has-error/);
        }
      }
    });
  });
}

test('update modal fits a small light window with long notes and Escape defers for this process',async({page})=>{
  await fixture(page,false);
  await page.getByRole('switch',{name:'黑夜模式'}).uncheck();
  await page.setViewportSize({width:650,height:480});
  await page.evaluate(()=>{Object.assign((window as any).widgetFixture.update,{phase:'available',version:'0.1.32',notes:'# Fusion 0.1.32\n## 本次重点\n'+Array(50).fill('- 统一更新操作并保留真实账号绑定。').join('\n'),startupPromptPending:true});});
  const dialog=page.getByRole('dialog',{name:'Fusion 软件更新'});
  await expect(dialog).toBeVisible();
  for(const name of ['稍后更新','立即更新']){
    const box=await dialog.getByRole('button',{name,exact:true}).boundingBox();
    expect(box).not.toBeNull();expect(box!.y).toBeGreaterThanOrEqual(0);expect(box!.y+box!.height).toBeLessThanOrEqual(480);
  }
  expect(await page.locator('.update-card-body').evaluate(e=>e.scrollHeight>e.clientHeight)).toBe(true);
  await page.screenshot({path:'test-results/updates-light-small.png',animations:'disabled'});
  await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0);
  await page.getByRole('button',{name:'账号控制台',exact:true}).click();
  await expect(dialog).toHaveCount(0);
});
