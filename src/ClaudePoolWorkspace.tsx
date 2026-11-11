import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ActionButton, AccountTool, PoolPrinter } from './Controls';
import { Quota } from './AccountQuota';
import { LaunchButton } from './LaunchButton';
import { Modal } from './Modal';
import { UsageDashboard, type DashboardUsage } from './UsageDashboard';
import type { Account, Tokens } from './PoolTelemetry';
import type { ApiRequestLog, ClientStatus, WorkspacePage } from './api-types';
import { quotaIsStale, quotaUpdatedLabel } from './quota';

type ClaudeAccount = Account & { tokens: Tokens; measuredRequests: number; requests: number };
type Status = { accounts: ClaudeAccount[]; preferred: string | null; summary?: {tokens:Tokens;requests:number;measuredRequests:number}; client: ClientStatus; login: {phase: string; message: string}; logs: ApiRequestLog[] };
const date = (n: number) => new Date(n * 1000).toLocaleString('zh-CN', { hour12: false });
const localDay = (d: Date) => `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,'0')}-${String(d.getDate()).padStart(2,'0')}`;
const count = (n?: number | null) => n == null ? '—' : n.toLocaleString('zh-CN');
const states: Record<string,string> = {ready:'可用',paused:'已暂停',reauth:'需重新授权',deleting:'移除未完成'};
export function ClaudePoolWorkspace({ page, onStatus, onCount, onDetails }: {
  page: WorkspacePage; onStatus: (s: ClientStatus) => void; onCount: (n: number) => void; onDetails: () => void;
}) {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState('');
  const [working, setWorking] = useState('');
  const [operation, setOperation] = useState('');
  const [removing, setRemoving] = useState<ClaudeAccount | null>(null);
  const [refreshing, setRefreshing] = useState<Record<string,boolean>>({});
  const mounted = useRef(false), reading = useRef(false), operating = useRef(false);
  const refresh = useCallback(async () => {
    if (reading.current) return;
    reading.current = true;
    try {
      const next = await invoke<Status>('claude_pool_status', { logs: true });
      if (mounted.current) { setStatus(next); onStatus(next.client); onCount(next.accounts.length); }
    } catch (e) { if (mounted.current) setError(String(e)); }
    finally { reading.current = false; }
  }, [onStatus,onCount]);
  useEffect(() => {
    mounted.current = true; void refresh();
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); }, 3000);
    return () => { mounted.current = false; window.clearInterval(timer); };
  }, [refresh]);
  async function run(label: string, command: string, args?: Record<string,unknown>) {
    if (operating.current) return;
    operating.current = true; setWorking(label); setOperation(command); setError('');
    try { await invoke(command,args); }
    catch (e) { if (mounted.current) setError(String(e)); }
    finally { if (mounted.current) await refresh(); operating.current = false; if (mounted.current) { setWorking(''); setOperation(''); } }
  }
  async function refreshQuota(id: string) {
    if (refreshing[id]) return;
    setRefreshing(old => ({...old,[id]:true})); setError('');
    try { await invoke('claude_pool_account',{action:'refresh',id}); }
    catch(e) { if (mounted.current) setError(String(e)); }
    finally { if (mounted.current) { setRefreshing(old => ({...old,[id]:false})); await refresh(); } }
  }
  const accounts = status?.accounts ?? [];
  const own = status?.client.sourceId === 'claude-subscription-pool';
  const running = !!own && status?.client.mode === 'running';
  const transitional = status?.client.mode === 'starting' || status?.client.mode === 'stopping';
  const pending = status?.login.phase === 'pending';
  const busy = !!working || !!transitional;
  const lifecycleWorking = !!transitional || operation === 'claude_pool_start' || operation === 'claude_pool_stop';
  const available = accounts.filter(a => a.available).length;
  const dashboard = useMemo<DashboardUsage | null>(() => {
    if (!status) return null;
    const tokens: Tokens = {inputTokens:0,outputTokens:0,totalTokens:0,cachedTokens:null,reasoningTokens:null};
    for (const a of status.accounts) for (const key of ['inputTokens','outputTokens','totalTokens'] as const) tokens[key] += a.tokens[key];
    if (status.summary) Object.assign(tokens,status.summary.tokens);
    const days = Array.from({length:7},(_,i) => { const d = new Date(); d.setDate(d.getDate()-6+i); const day=localDay(d); const logs=status.logs.filter(l => localDay(new Date(l.at*1000))===day && l.usage); return {date:day,totalTokens:logs.length ? logs.reduce((s,l) => s+(l.usage?.totalTokens ?? 0),0) : null}; });
    return {tokens,measured:status.summary ? status.summary.measuredRequests>0 : status.accounts.some(a => a.measuredRequests>0),days,hours:[],today:localDay(new Date()),timezone:'本地时间',updatedAt:Date.now(),scanning:false,historyPartial:true,activityPartial:true,error:null};
  },[status]);
  const stop = running || own && status?.client.mode !== 'off';
  const action = (action: string,id: string | null) => void run('正在更新账号…','claude_pool_account',{action,id});
  return <div className="claude-pool-workspace">
    {error && <section role="alert"><p>{error}</p><button onClick={() => {setError('');void refresh();}}>刷新状态</button></section>}
    {working && <p role="status">{working}</p>}
    {status?.client.error && <p role="alert">{status.client.error}</p>}
    {pending && <section className="login-actions"><p role="status">{status?.login.message}</p><button onClick={() => void run('正在取消登录…','claude_pool_cancel_login')}>取消登录</button></section>}
    {!pending && status?.login.message && <p className="quota-refresh-note" role="status">{status.login.message}</p>}
    {page === 'accounts' && <div className="accounts-page"><div className="dashboard-summary">
      <section className="launch-panel"><div className="launch-copy"><h2>{running ? '账号池已启动' : transitional ? '账号池正在切换' : own && status?.client.mode === 'recoveryRequired' ? '账号池需要恢复' : '账号池未启动'}</h2><div className="button-row"><LaunchButton label={busy ? '正在处理…' : stop ? '关闭账号池' : '启动账号池'} running={running} transitioning={lifecycleWorking} failed={status?.client.mode==='recoveryRequired'} disabled={busy || !status || !stop && (!available || !!pending)} onClick={() => void run(stop ? '正在排空请求并关闭 Claude…' : '正在检查并启动 Claude…',stop ? 'claude_pool_stop' : 'claude_pool_start')} /></div>{!available && <p role="status">{accounts.length ? '没有可用账号，请检查额度或授权。' : '请先添加 Claude 订阅账号。'}</p>}</div><PoolPrinter running={running} /></section>
      <div className="overview-line"><span>可用账号 <b>{available} / {accounts.length}</b></span><span>{status?.preferred ? '手动优先' : '自动分配'}</span><span>用量已上报 <b>{count(status?.summary?.measuredRequests ?? accounts.reduce((s,a)=>s+a.measuredRequests,0))} / {count(status?.summary?.requests ?? accounts.reduce((s,a)=>s+a.requests,0))}</b> 次请求</span></div>
      </div><div className="dashboard-grid"><UsageDashboard data={dashboard} onDetails={onDetails} label="Claude 账号池用量" caption="Claude 账号池用量" /></div>
      <div className="accounts-section"><div className="section-head accounts-heading"><div><h2>我的账号 <span className="count">{accounts.length}</span></h2><p className="quota-refresh-note">每 10 分钟自动更新 · 可随时手动刷新</p></div><div className="button-row"><button disabled={busy || !status?.preferred} onClick={() => action('select',null)}>自动优先</button><button disabled={busy || Object.values(refreshing).some(Boolean) || !accounts.length} onClick={() => void Promise.all(accounts.filter(a => !['reauth','deleting'].includes(a.state)).map(a => refreshQuota(a.id)))}>刷新全部额度</button><ActionButton kind="add" className="primary" disabled={busy || pending} onClick={() => void run('正在打开 Claude 登录…','claude_pool_login',{id:null})}>添加账号</ActionButton></div></div>
        <div className="account-list">{accounts.map(a => {
          const selected=status?.preferred===a.id;
          const stale=!!a.quotaError || !!a.quota && quotaIsStale(a.quota.updatedAt);
          const cooling=a.cooldownUntil>Date.now()/1000;
          return <article key={a.id} className={'account'+(selected ? ' active-account' : '')}><div className="account-banner"><span className="account-notch">CLAUDE / 账号</span><div className="account-tools"><AccountTool tool="refresh" title="刷新额度" aria-label={`刷新 ${a.email} 额度`} disabled={!!refreshing[a.id] || a.state==='deleting'} onClick={() => void refreshQuota(a.id)} /><AccountTool tool="auth" title="重新授权" aria-label={`重新授权 ${a.email}`} disabled={busy || pending} onClick={() => void run('正在打开重新授权…','claude_pool_login',{id:a.id})}/><AccountTool tool={a.state==='paused' ? 'resume' : 'pause'} title={a.state==='paused' ? '恢复账号' : '暂停账号'} aria-label={`${a.state==='paused' ? '恢复' : '暂停'} ${a.email}`} disabled={busy || ['reauth','deleting'].includes(a.state)} onClick={() => action(a.state==='paused' ? 'resume' : 'pause',a.id)} /></div></div>
            <div className="account-top"><div className="avatar">{a.email[0]?.toUpperCase()}</div><div className="account-info"><h3>{a.email}</h3><span>{a.planLabel}</span>{selected && <div className="account-selection-tags"><em>当前首选</em></div>}</div><span className={'badge'+(!a.available ? ' warning' : '')}>{cooling ? '冷却中' : a.state==='ready' && !a.available ? '额度不足' : states[a.state] ?? a.state}</span></div>
            <div className="quota-grid"><Quota data={a.quota?.rateLimit?.primary_window} fallback="5 小时周期" stale={stale}/><Quota data={a.quota?.rateLimit?.secondary_window} fallback="7 天周期" stale={stale}/></div>
            <div className="quota-meta"><div className="quota-update" role="status">{refreshing[a.id] ? '正在刷新额度…' : a.quota ? quotaUpdatedLabel(a.quota.updatedAt)+(stale ? ' · 上次数据' : '') : '等待额度查询'}</div>{a.quotaError && <span className="warning-text">{a.quotaError}</span>}</div>
            <div className="account-tokens"><span>累计 Token <b>{a.measuredRequests ? count(a.tokens.totalTokens) : '—'}</b></span><span>输入 <b>{a.measuredRequests ? count(a.tokens.inputTokens) : '—'}</b></span><span>输出 <b>{a.measuredRequests ? count(a.tokens.outputTokens) : '—'}</b></span></div>
            <ActionButton kind="start" className="account-enable" disabled={busy || !a.available || selected} onClick={() => action('select',a.id)}>{selected ? '当前首选账号' : '使用此账号'}</ActionButton><div className="account-actions"><span className="quota-refresh-note">已有会话保持原账号</span><ActionButton kind="delete" aria-label={`移除 ${a.email}`} disabled={busy || status?.client.mode!=='off'} onClick={() => setRemoving(a)}>移除账号</ActionButton></div>
          </article>;
        })}</div>
        {!accounts.length && <section className="empty"><h3>还没有 Claude 账号</h3><p>添加自己的订阅账号后，可查看额度并启动独立账号池。</p></section>}
      </div></div>}
    {page === 'service' && <div className="service-grid"><section><h2>Claude 订阅账号池</h2><p>独立管理 Claude 登录、额度与用量。新会话按首选账号分配，已有会话、工具结果和签名上下文保持原账号。</p><p>网络使用当前系统代理；不使用 Codex 订阅池的专用节点。</p><p>真实 Claude 客户端登录、额度及多轮请求仍待验收。</p></section></div>}
    {page === 'logs' && <section><div className="section-head"><h2>Claude 请求记录</h2><button onClick={() => void refresh()}>刷新记录</button></div>{!status?.logs.length ? <p className="empty">暂无账号池请求记录</p> : <div className="cw-logs">{status.logs.map(log => <article className="cw-log-row" key={log.id}><strong>{log.model || '模型未记录'}</strong><span>{accounts.find(a=>a.id===log.sourceId)?.email || '已移除账号'}</span><span>{log.status}</span><span>{log.usage ? count(log.usage.totalTokens)+' Token' : '用量未返回'}</span><time>{date(log.at)}</time></article>)}</div>}</section>}
    {removing && <Modal label="移除 Claude 账号" busy={busy} onClose={() => setRemoving(null)}><div className="modal small"><h2>移除 {removing.email}？</h2><p>移除账号将使其旧会话无法续接，重新添加不会恢复原绑定。临时停用请使用暂停。</p><div className="button-row"><button onClick={() => setRemoving(null)}>取消</button><button disabled={busy} onClick={() => {action('remove',removing.id);setRemoving(null);}}>确认移除</button></div></div></Modal>}
  </div>;
}
