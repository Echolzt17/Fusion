import { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { PoolWidget } from './PoolWidget';
import { ActiveRequests, type ActiveRequest } from './ActiveRequests';
import { Modal } from './Modal';
import { NotificationCenter, useNotifications } from './NotificationCenter';
import { type NoticeInput } from './notifications';
import { createReadCoordinator } from './refresh';
import { quotaIsStale, quotaUpdatedLabel } from './quota';
import './style.css';
import './account-selection.css';
import './conversation-guide.css';
import './sidebar.css';
import { WindowChrome } from './WindowChrome';
import { ActionButton, ThemeSwitch, PoolPrinter, AccountTool } from './Controls';
import { RequestRecords, type HistoryIndex } from './RequestRecords';
import { type Account, type Usage, type Log } from './PoolTelemetry';

import { PrioritySelector, type SelectionPolicy, type SchedulingStatus } from './PrioritySelector';
import { useUpdates, UpdateButton, UpdateDialog, RepositoryButton } from './UpdatePanel';
import { LaunchButton } from './LaunchButton';
import { Quota } from './AccountQuota';
import { ClaudePoolWorkspace } from './ClaudePoolWorkspace';
import { SubscriptionCountdown } from './SubscriptionCountdown';
import { ErrorReport } from './ErrorReport';
import { UsageDashboard, type DashboardUsage } from './UsageDashboard';
import { NetworkCards } from './NetworkCards';
import { LocalProjectsCard } from './LocalProjectsCard';
import { PricingCards, usePricing } from './PricingCards';
import { WorkspaceSwitcher, ApiWorkspace, type ClientId, type WorkspaceView, type ClientStatus } from './ClientWorkspace';

type ResetCredits = { available_count: number; credits: { id: string; reset_type: string; status: string; expires_at: string | null }[] };
type Settings = { port: number; concurrency: number; fixedAccount: string | null; selectionPolicy: SelectionPolicy };
type Service = { running: boolean; draining: boolean; activeRequests: number; baseUrl: string; storageWarning?: string | null; requests?: ActiveRequest[] };
type Login = { phase: string; message: string };
type ImageCompatibility = { client: { desktop: string | null; engine: string | null }; verificationState: 'pending' | 'checking' | 'verified' | 'failed' | 'unavailable'; detail: string; isolatedVerified: boolean; configurationReady: boolean; routesReady: boolean; upstreamImageReceived: boolean; desktopAcceptance: 'pending' };
type Connection = { mode: 'off' | 'on' | 'enabling' | 'closing' | 'recovery_required' | 'unknown'; configured: boolean; recoveryPending: boolean; reloadRequired: string | null; error: string | null; operation: 'start' | 'stop' | null; phase?: 'catalog_queue' | 'catalog_auth' | 'catalog_proxy' | 'catalog_headers' | 'catalog_body' | 'checking' | 'compatibility' | 'configuring' | 'restoring' | 'draining' | 'quitting' | 'terminating' | 'launching' | 'verifying' | 'failed' | null; operationStartedAt?: number | null };

const labels: Record<string, string> = { ready: '可用', paused: '已暂停', reauth: '需重新授权', deleting: '移除未完成，请重试移除', success: '成功', cancelled: '已取消', stream_interrupted: '流中断', upstream_rate_limited: '账号限流', account_unavailable: '账号不可用' };
const date = (n: number) => new Date(n * 1000).toLocaleString('zh-CN', { hour12: false });
const num = (n: number) => n.toLocaleString('zh-CN');
function Icon({ name }: { name: 'power' | 'grid' | 'activity' | 'server' | 'plus' | 'arrow' }) {
  const paths = { power: 'M12 3v9M6.3 5.8a8 8 0 1 0 11.4 0', grid: 'M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z', activity: 'M2 12h4l3-8 6 16 3-8h4', server: 'M3 4h18v6H3zM3 14h18v6H3zM7 7h1M7 17h1', plus: 'M12 4v16M4 12h16', arrow: 'M5 12h14M13 6l6 6-6 6' };
  return <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={paths[name]} /></svg>;
}
function App() {
  const pricing = usePricing();
  const notifications = useNotifications();
  const inbox = notifications.store;
  const notify = useCallback((notice: { text: string; error: boolean }) => inbox.push({ title: notice.error ? '操作未完成' : notice.text.split(/[。；\n]/)[0].slice(0, 40), message: notice.text, level: notice.error ? 'error' : 'success' }), [inbox]);
  const [dark, setDark] = useState(() => { try { const saved = localStorage.getItem('pool-theme'); return saved ? saved === 'dark' : window.matchMedia('(prefers-color-scheme: dark)').matches; } catch { return true; } });
  useEffect(() => { document.documentElement.dataset.theme = dark ? 'dark' : 'light'; try { localStorage.setItem('pool-theme', dark ? 'dark' : 'light'); } catch {} }, [dark]);
  const [page, setPage] = useState<'accounts' | 'service' | 'logs'>('accounts');
  const [workspace, setWorkspace] = useState<WorkspaceView>('codex');
  const [apiClient, setApiClient] = useState<ClientId>('codex');
  const [apiStatuses, setApiStatuses] = useState<ClientStatus[]>([]);
  const [claudeAccountCount, setClaudeAccountCount] = useState(0);
  const acceptClaudeStatus = useCallback((status: ClientStatus) => setApiStatuses(old => [...old.filter(s => s.client !== 'claudeCode'), status]), []);
  const client = workspace === 'api' ? apiClient : workspace;
  const showPool = workspace === 'codex';
  const pageRef = useRef(page); pageRef.current = page;
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [settings, setSettings] = useState<Settings>({ port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' });
  const schedulingRevision = useRef(0);
  const [scheduling, setScheduling] = useState<SchedulingStatus>({ policy: 'auto', preferredAccountId: null, currentAccountId: null });
  const [service, setService] = useState<Service>({ running: false, draining: false, activeRequests: 0, baseUrl: 'http://127.0.0.1:18765/v1' });
  const [readState, setReadState] = useState<Record<string, { error: string | null; at?: number }>>({});
  const [shutdown, setShutdown] = useState<'stop' | 'exit' | null>(null);
  const [retryTarget, setRetryTarget] = useState<'stop' | 'exit'>('stop');
  const [clock, setClock] = useState(Date.now());
  const [login, setLogin] = useState<Login>({ phase: 'idle', message: '' });
  const [logs, setLogs] = useState<Log[]>([]);
  const [historyIndex, setHistoryIndex] = useState<HistoryIndex>({conversations:[]});
  const [usage, setUsage] = useState<Usage[]>([]);
  const [dashboardUsage, setDashboardUsage] = useState<DashboardUsage | null>(null);
  const [connection, setConnection] = useState<Connection>({ mode: 'unknown', configured: false, recoveryPending: false, reloadRequired: null, error: null, operation: null });
  useEffect(() => { if (!shutdown && connection.mode !== 'closing' && connection.mode !== 'enabling') return; const t = setInterval(() => setClock(Date.now()), 1000); return () => clearInterval(t); }, [shutdown, connection.mode]);
  const poolOn = connection.mode === 'on';
  const transitioning = connection.mode === 'enabling' || connection.mode === 'closing';
  const [imageCompatibility, setImageCompatibility] = useState<ImageCompatibility | null>(null);
  const [operationBusy, setBusy] = useState(false);
  const [updating, setUpdating] = useState(false);
  const updates = useUpdates(setUpdating);
  const busy = operationBusy || updating || transitioning || shutdown !== null;
  const quotaReads = useRef(new Map<string, Promise<void>>());
  const [quotaRefreshing, setQuotaRefreshing] = useState<Record<string, boolean>>({});
  const quotaBatch = useRef<Promise<void> | null>(null);
  const [allQuotaRefreshing, setAllQuotaRefreshing] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [removeId, setRemoveId] = useState<string | null>(null);
  const [resetAccount, setResetAccount] = useState<Account | null>(null);
  const [resetInfo, setResetInfo] = useState<Record<string, { credits?: ResetCredits; error?: string; loading?: boolean }>>({});
  const resetReads = useRef(new Map<string, Promise<ResetCredits | null>>());
  const resetChecked = useRef(new Set<string>());
  const [creditId, setCreditId] = useState('');
  async function loadResets(id: string) {
    const existing = resetReads.current.get(id); if (existing) return existing;
    resetChecked.current.add(id);
    setResetInfo(v => ({ ...v, [id]: { ...v[id], loading: true, error: undefined } }));
    const pending = invoke<ResetCredits>('reset_credits', { id }).then(credits => {
      setResetInfo(v => ({ ...v, [id]: { credits } })); return credits;
    }).catch(e => { setResetInfo(v => ({ ...v, [id]: { ...v[id], loading: false, error: String(e) } })); return null; })
      .finally(() => resetReads.current.delete(id));
    resetReads.current.set(id, pending); return pending;
  }
  useEffect(() => {
    if (page !== 'accounts') return;
    // Query each account once when first shown, independently of local polling.
    // A slow/failing card query never holds the rest of the account information.
    for (const a of accounts) if (a.state !== 'reauth' && a.state !== 'deleting' && !resetChecked.current.has(a.id)) void loadResets(a.id);
  }, [accounts, page]);

  useEffect(() => {
    const conditions: { key: string; notice: NoticeInput }[] = [];
    const add = (key: string, title: string, message: string | null | undefined, level: NoticeInput['level'] = 'error') => { if (message) conditions.push({ key, notice: { title, message, level } }); };
    for (const [name, state] of Object.entries(readState)) if (state.error) add(`read:${name}`, `${name}读取失败`, `${state.error}；${state.at ? `最后成功 ${new Date(state.at).toLocaleTimeString('zh-CN')}` : '尚无有效数据'}`);
    add('storage', '请求统计保存不完整', service.storageWarning, 'warning');
    if (startError || connection.phase === 'failed') add('switch', startError || connection.operation === 'start' ? '启动未完成' : '关闭未完成', startError || connection.error || '请重试。');
    else if (connection.mode === 'recovery_required') add('recovery', '账号池接入需要恢复', '请在账号控制台恢复原接入并关闭。', 'warning');
    else add('connection-error', '账号池接入异常', connection.error);
    if (login.phase === 'error' || login.phase === 'success') add('login', login.phase === 'error' ? '登录未完成' : '账号已添加', login.message, login.phase === 'error' ? 'error' : 'success');
    inbox.sync('status', conditions);
  }, [inbox, readState, service.storageWarning, connection, login, startError]);

  async function openShutdown(target: 'stop' | 'exit') {
    setStartError(null); setShutdown(target); setRetryTarget(target); setClock(Date.now());
    try { await invoke('close_pool', { exitApp: target === 'exit' }); }
    catch (e) { setShutdown(null); setConnection(c => ({ ...c, operation: 'stop', phase: 'failed', error: String(e) })); }
  }
  useEffect(() => {
    const requestClose = () => { void openShutdown('stop').catch(e => notify({ text: String(e), error: true })); };
    const listeners = [
      listen<string | null>('desktop-warning', e => inbox.sync('desktop-widget', e.payload ? [{ key: 'widget', notice: { title: 'Codex 挂件不可用', message: e.payload, level: 'warning' } }] : [])),
      listen('widget-close-pool', () => { setPage('service'); requestClose(); }),
      listen('widget-open-console', () => setPage('accounts')),
      listen('prepare-uninstall', () => { setPage('service'); requestClose(); }),
      listen('start-complete', () => { setStartError(null); reader.current?.invalidate(); setConnection({mode:'on',configured:true,recoveryPending:true,reloadRequired:null,error:null,operation:null,phase:null,operationStartedAt:null}); inbox.push({ title: '账号池已启动', message: 'Codex 已完成重新启动，可以使用账号池。', level: 'success' }); refresh(); }),
      listen<string>('start-failed', e => { setStartError(e.payload); reader.current?.invalidate(); setConnection(c => ({...c,mode:c.configured?'recovery_required':'off',operation:'start',phase:'failed',error:e.payload})); }),
      listen('close-complete', () => { setStartError(null); reader.current?.invalidate(); setShutdown(null); setService(s=>({...s,running:false,draining:false,activeRequests:0})); setConnection({ mode:'off',configured:false,recoveryPending:false,operation:null,phase:null,operationStartedAt:null,error:null,reloadRequired:null }); inbox.push({ title: '账号池已关闭', message: '原接入已恢复，Codex 已关闭。继续池内旧聊天需重新启动账号池并保留原账号；改用官方账号时，请手动打开 Codex，新建聊天并带入任务摘要。', level: 'success' }); }),
      listen<{message: string; exitApp: boolean}>('close-failed', e => { setShutdown(null); setRetryTarget(e.payload.exitApp ? 'exit' : 'stop'); setConnection(c => ({ ...c, mode: 'recovery_required', operation: 'stop', phase: 'failed', error: e.payload.message })); }),
    ];
    void invoke<boolean>('uninstall_requested').then(requested => { if (requested) { setPage('service'); requestClose(); } }).catch(() => {});
    return () => { for (const listener of listeners) void listener.then(unlisten => unlisten()); };
  }, []);
  const reader = useRef<ReturnType<typeof createReadCoordinator> | null>(null);
  if (!reader.current) reader.current = createReadCoordinator((name, error) => setReadState(previous => ({ ...previous, [name]: { error, at: error ? previous[name]?.at : Date.now() } })));
  const refresh = useCallback(() => {
    const revision = schedulingRevision.current;
    reader.current!.refresh([
      { name: '账号', read: () => invoke<Account[]>('accounts'), update: setAccounts },
      { name: '服务', read: () => invoke<Service>('service_status'), update: setService },
      { name: '登录', read: () => invoke<Login>('login_status'), update: setLogin },
      { name: '日志', read: () => invoke<Log[]>('logs'), update: value => { setLogs(value); inbox.ingestLogs(value); } },
      ...(pageRef.current === 'logs' ? [{ name: '对话统计', read: () => invoke<HistoryIndex>('conversation_usage'), update: setHistoryIndex }] : []),
      ...(pageRef.current === 'accounts' ? [{ name: '用量', read: () => invoke<Usage[]>('usage'), update: setUsage }] : []),
      ...(pageRef.current === 'accounts' ? [{ name: '本机用量', read: () => invoke<DashboardUsage>('usage_dashboard'), update: setDashboardUsage }] : []),
      { name: '账号池接入', read: () => invoke<Connection>('connection_status'), update: setConnection },
      { name: 'API连接', read: () => invoke<ClientStatus[]>('api_client_status'), update: value => setApiStatuses(Array.isArray(value) ? value : []) },
      { name: '调度', read: () => invoke<SchedulingStatus>('scheduling_status'), update: s => { if (revision === schedulingRevision.current) setScheduling(s); } },
      { name: '生图兼容性', read: () => invoke<ImageCompatibility>('image_compatibility'), update: setImageCompatibility },
    ]);
  }, [inbox]);
  useEffect(() => { refresh(); }, [page, refresh]);
  useEffect(() => {
    let disposed = false; let checking = false;
    invoke<Settings>('settings').then(s => { if (!disposed) setSettings(s); }).catch(e => notify({ text: String(e), error: true }));
    const poll = async () => {
      if (checking || disposed || document.hidden) return;
      checking = true;
      try { if (await invoke<boolean>('main_visible') && !disposed) refresh(); inbox.sync('visibility', []); }
      catch (e) { if (!disposed) inbox.sync('visibility', [{ key: 'error', notice: { title: '窗口状态读取失败', message: String(e), level: 'error' } }]); }
      finally { checking = false; }
    };
    const visible = listen<boolean>('main-visible', event => { if (event.payload) void poll(); });
    document.addEventListener('visibilitychange', poll);
    const t = setInterval(() => void poll(), 3000);
    return () => { disposed = true; clearInterval(t); reader.current?.dispose(); document.removeEventListener('visibilitychange', poll); void visible.then(unlisten => unlisten()); };
  }, [refresh]);
  async function run(work: () => Promise<unknown>, message?: string) {
    setBusy(true); reader.current!.invalidate();
    try { await work(); reader.current!.invalidate(); refresh(); if (message) notify({ text: message, error: false }); }
    catch (e) { notify({ text: String(e), error: true }); refresh(); }
    finally { setBusy(false); }
  }
  function refreshQuota(id: string): Promise<void> {
    const existing = quotaReads.current.get(id);
    if (existing) return existing;
    setQuotaRefreshing(previous => ({ ...previous, [id]: true }));
    void loadResets(id);
    const pending = invoke<void>('refresh_quota', { id }).finally(() => {
      quotaReads.current.delete(id);
      setQuotaRefreshing(previous => { const next = { ...previous }; delete next[id]; return next; });
      reader.current!.invalidate(); refresh();
    });
    quotaReads.current.set(id, pending);
    return pending;
  }
  function refreshAllQuotas() {
    if (quotaBatch.current) return;
    const targets = accounts.filter(account => account.state !== 'deleting' && account.state !== 'reauth');
    if (!targets.length) return;
    setAllQuotaRefreshing(true);
    const pending = Promise.allSettled(targets.map(account => refreshQuota(account.id))).then(results => {
      const failed = results.filter(result => result.status === 'rejected').length;
      inbox.push({
        title: failed ? '额度刷新完成，部分账号未更新' : '额度刷新完成',
        message: failed ? `成功 ${results.length - failed} 个，失败 ${failed} 个；失败账号保留上次结果，可稍后单独重试。` : `已刷新 ${results.length} 个账号的额度。`,
        level: failed ? 'warning' : 'success',
      });
    }).finally(() => { quotaBatch.current = null; setAllQuotaRefreshing(false); });
    quotaBatch.current = pending;
  }
  async function changePolicy(policy: SelectionPolicy): Promise<boolean> {
    setBusy(true); reader.current!.invalidate();
    try { await invoke('set_selection_policy', { policy }); schedulingRevision.current++; setScheduling(s => ({ ...s, policy, preferredAccountId: null })); reader.current!.invalidate(); refresh(); return true; }
    catch (e) { notify({ text: String(e), error: true }); return false; }
    finally { setBusy(false); }
  }
  async function probe() {
    return invoke<string>('probe');
  }
  async function activate(id?: string) {
    setStartError(null);
    if (transitioning) throw new Error('账号池正在切换，请等待完成');
    if (poolOn && imageCompatibility?.configurationReady && connection.phase !== 'failed') {
      if (id) { await invoke('select_account', { accountId: id }); schedulingRevision.current++; setSettings(await invoke<Settings>('settings')); }
      notify({ text: id ? '新会话优先使用此账号，已有会话保持原账号。' : await probe(), error: false });
      return;
    }
    setConnection(c => ({...c,mode:'enabling',operation:'start',phase:'checking',operationStartedAt:Math.floor(Date.now()/1000),error:null}));
    try { await invoke('start_pool', { accountId: id ?? null }); }
    catch (e) { setConnection(c => ({...c,mode:c.configured?'recovery_required':'off',operation:'start',phase:'failed',error:String(e)})); setStartError(String(e)); }
  }
  const pending = login.phase === 'pending';
  const available = accounts.filter(a => a.available).length;
  const retryStart = !!startError || (connection.operation === 'start' && connection.phase === 'failed');
  const needsEnable = retryStart || (!poolOn && connection.mode !== 'recovery_required');
  const activationBlocked = connection.mode === 'unknown' ? (readState['账号池接入']?.error ? `接入状态读取失败：${readState['账号池接入'].error}` : '正在读取接入状态…') : needsEnable && pending ? '请先完成或取消当前账号登录。' : needsEnable && !available ? (accounts.length ? '没有可用账号，请检查账号是否已暂停或需要重新授权。' : '请先添加账号，再启动账号池。') : null;
  const poolActionDisabled = busy || transitioning || !!activationBlocked;
  const phaseLabel: Record<string,string> = {catalog_queue:'等待账号名额…',catalog_auth:'正在检查或续期授权…',catalog_proxy:'正在解析当前代理…',catalog_headers:'等待上游模型目录响应…',catalog_body:'正在读取模型目录…',checking:'正在检查客户端…',compatibility:'正在自动检查客户端兼容性，首次启动或更新后可能需要约一分钟…',configuring:'正在配置账号池…',restoring:'正在恢复原接入…',draining:'等待已有请求完成…',quitting:'正在退出 Codex…',terminating:'正在结束 Codex 后台…',launching:'正在打开 Codex…',verifying:'正在核验客户端…'};
  const failed = !!startError || connection.phase === 'failed';
  const poolActionLabel = transitioning ? (connection.mode === 'enabling' ? '正在启动…' : '正在关闭…') : failed ? (retryStart ? '重试启动' : '重试关闭') : poolOn || connection.mode === 'recovery_required' ? '关闭账号池' : '启动账号池';
  const poolAction = () => retryStart || (!poolOn && connection.mode !== 'recovery_required') ? activate() : openShutdown(retryTarget);

  const measured = usage.reduce((s, u) => s + u.measuredRequests, 0);
  const requests = usage.reduce((s, u) => s + u.requests, 0);
  const activeLabel = page === 'accounts' ? (showPool ? '账号控制台' : '账号与来源') : page === 'service' ? '连接与设置' : '请求日志';
  const apiStatus = apiStatuses.find(status => status.client === client && status.mode !== 'off');
  const apiStateNames = { starting: 'API 正在启动', running: 'API 运行中', stopping: 'API 正在关闭', recoveryRequired: 'API 接入需要恢复', failed: 'API 操作未完成', off: 'API 已关闭' };
  return <div className="app">
    <WindowChrome />
    <aside>
      <nav className="sidebar-nav" aria-label="主导航">{([['accounts', 'grid', '账号控制台'], ['service', 'server', '连接与设置'], ['logs', 'activity', '请求日志']] as const).map(([id, icon, name]) => <button key={id} className={page === id ? 'selected' : ''} aria-label={name} aria-current={page === id ? 'page' : undefined} title={name} onClick={() => setPage(id)}><Icon name={icon} />{name}{id === 'accounts' && <em>{workspace === 'claudeCode' ? claudeAccountCount : accounts.length}</em>}</button>)}</nav>
      <div className="sidebar-bottom"><RepositoryButton controller={updates} /></div>
    </aside>
    <main>
      <header className="cw-app-header"><div className="cw-workspace-heading"><WorkspaceSwitcher value={workspace} onChange={setWorkspace} />{page !== 'accounts' && <h1 className="cw-page-label">{activeLabel}</h1>}</div><div className="header-actions"><UpdateButton controller={updates} /><NotificationCenter {...notifications} /><ThemeSwitch dark={dark} onChange={() => setDark(v => !v)} /><span className="local-tag"><span className={'dot ' + (apiStatus?.mode === 'running' || client === 'codex' && service.running ? 'on' : '')} />{workspace === 'claudeCode' && apiStatus?.sourceId === 'claude-subscription-pool' ? (apiStatus.mode === 'running' ? '账号池已启动' : apiStatus.mode === 'starting' ? '正在启动账号池' : apiStatus.mode === 'stopping' ? '正在关闭账号池' : '接入需要恢复') : apiStatus ? apiStateNames[apiStatus.mode] : client === 'claudeCode' ? '账号池已关闭' : poolOn ? '账号池已启动' : connection.mode === 'enabling' ? '正在启动账号池' : connection.mode === 'closing' ? '正在关闭账号池' : connection.mode === 'recovery_required' ? '接入需要恢复' : connection.mode === 'off' ? '账号池已关闭' : '正在读取接入状态'}</span></div></header>
      {(shutdown || transitioning) && <section role="status" aria-live="polite" className="shutdown-progress"><h2>{shutdown === 'exit' ? '正在退出 Fusion' : connection.mode === 'enabling' ? '正在启动账号池' : '正在关闭账号池'}</h2><p>{phaseLabel[connection.phase || ''] || '正在准备切换…'} · 活动请求 {service.activeRequests} 个 · 已等待 {connection.operationStartedAt ? Math.max(0, Math.floor(clock / 1000 - connection.operationStartedAt)) : 0} 秒</p></section>}
      {failed && <section role="alert"><h2>{retryStart ? '启动未完成' : '关闭未完成'}</h2><p>{startError || connection.error || '客户端切换尚未完成，请重试。'}</p>{retryStart && (connection.configured || connection.recoveryPending || poolOn) && <button disabled={busy || transitioning} onClick={() => void run(() => openShutdown('stop'))}>关闭账号池</button>}</section>}
      {apiStatus && workspace !== 'claudeCode' && <div className="conversation-guide" role="status">{client === 'codex' ? 'Codex' : 'Claude Code'} 当前接入：{apiStateNames[apiStatus.mode]}{apiStatus.error ? ` · ${apiStatus.error}` : ''}{showPool ? '。启动账号池将等待当前 API 请求完成并切换接入。' : ''}</div>}
      {workspace === 'api' && <ApiWorkspace client={apiClient} onClientChange={setApiClient} page={page} poolMode={poolOn} poolBusy={updating} onSwitchToPool={() => setWorkspace('codex')} />}
      {workspace === 'claudeCode' && <ClaudePoolWorkspace page={page} onStatus={acceptClaudeStatus} onCount={setClaudeAccountCount} onDetails={() => setPage('logs')} />}
      {showPool && page === 'accounts' && <div className="accounts-page"><div className="dashboard-summary">
        <section className="launch-panel"><div className="launch-copy"><h2>{poolOn ? '账号池已启动' : connection.mode === 'enabling' ? '账号池启动中' : connection.mode === 'closing' ? '账号池关闭中' : connection.mode === 'recovery_required' ? '账号池需要恢复' : '账号池未启动'}</h2><div className="button-row"><LaunchButton label={poolActionLabel} running={poolOn} transitioning={transitioning} failed={failed} disabled={poolActionDisabled} aria-describedby={activationBlocked ? 'launch-blocked' : undefined} onClick={() => void run(poolAction)} /></div>{activationBlocked && <p id="launch-blocked" role="status">{activationBlocked}</p>}</div><PoolPrinter running={poolOn} /></section>
        <div className="overview-line"><span>可用账号 <b>{available} / {accounts.length}</b></span><span>{scheduling.preferredAccountId ? '手动优先' : '策略分配'}</span><span>用量已上报 <b>{num(measured)} / {num(requests)}</b> 次请求</span></div>
        </div><div className="dashboard-grid"><UsageDashboard data={dashboardUsage} onDetails={() => setPage('logs')} />
        </div>
        <div className="accounts-section"><div className="section-head accounts-heading"><div><h2>我的账号 <span className="count">{accounts.length}</span></h2><p className="quota-refresh-note">每 10 分钟自动更新 · 可随时手动刷新{allQuotaRefreshing && <span role="status"> · 正在刷新全部额度…</span>}</p></div><div className="button-row"><PrioritySelector value={scheduling.policy} disabled={busy} onChange={changePolicy} /><button disabled={busy || allQuotaRefreshing || !accounts.some(a => a.state !== 'deleting' && a.state !== 'reauth')} aria-busy={allQuotaRefreshing} onClick={refreshAllQuotas}>刷新全部额度</button><ActionButton kind="add" className="primary" disabled={busy || pending} onClick={() => void run(() => invoke('login', { accountId: null }))}>添加账号</ActionButton></div></div>
        {pending && <div className="login-actions"><button disabled={busy} onClick={() => void run(() => invoke('cancel_login'))}>取消登录</button></div>}
        {accounts.length === 0 ? <section className="empty"><div className="empty-icon"><Icon name="plus" /></div><h3>尚无账号</h3></section> : <div className="account-list">{accounts.map(a => {
          const cooling = a.cooldownUntil > Date.now() / 1000;
          const u = usage.find(u => u.account === a.id);
          const selected = scheduling.currentAccountId === a.id;
          const preferred = scheduling.preferredAccountId === a.id;
          const refreshing = !!quotaRefreshing[a.id] || !!a.quotaRefreshing;
          const stale = !!a.quotaError || !!a.quota && quotaIsStale(a.quota.updatedAt);
          return <article className={'account ' + (selected ? 'active-account' : '')} key={a.id}><div className="account-banner"><span className="account-notch">CODEX / 账号</span><div className="account-tools"><AccountTool tool="refresh" title={refreshing ? '正在刷新额度' : '刷新额度'} aria-label="刷新额度" aria-busy={refreshing} disabled={busy || refreshing || a.state === 'deleting' || a.state === 'reauth'} onClick={() => void refreshQuota(a.id).catch(e => notify({ text: String(e), error: true }))} /><AccountTool tool="auth" title="重新授权" aria-label="重新授权" disabled={busy || pending || a.state === 'deleting'} onClick={() => void run(() => invoke('login', { accountId: a.id }))} /><AccountTool tool={a.state === 'paused' ? 'resume' : 'pause'} title={a.state === 'paused' ? '恢复账号' : '暂停账号'} aria-label={a.state === 'paused' ? '恢复账号' : '暂停账号'} disabled={busy || a.state === 'reauth' || a.state === 'deleting'} onClick={() => void run(() => invoke('pause_account', { id: a.id, paused: a.state !== 'paused' }))} /></div></div><div className="account-top"><div className="avatar">{a.email[0]?.toUpperCase()}</div><div className="account-info"><h3>{a.email}</h3><span>{a.planLabel || '套餐未知'}</span>{(selected || preferred) && <div className="account-selection-tags">{selected && <em>当前首选</em>}{preferred && <em>手动优先</em>}</div>}</div><span className={'badge ' + (a.state !== 'ready' || cooling ? 'warning' : '')}>{cooling && a.state === 'ready' ? '冷却中' : labels[a.state] || a.state}</span></div>
            <div className="quota-grid"><Quota data={a.quota?.rateLimit?.primary_window} fallback="短周期" stale={stale} /><Quota data={a.quota?.rateLimit?.secondary_window} fallback="长周期" stale={stale} /></div>
            <div className="quota-meta"><SubscriptionCountdown account={a} /><div className="quota-update" aria-live="polite">{refreshing && <span className="quota-refreshing"><i className="spinner" aria-hidden="true" />更新中</span>}{a.quota ? <time className={stale ? 'is-stale' : ''} dateTime={new Date(a.quota.updatedAt * 1000).toISOString()} title={`查询时间：${date(a.quota.updatedAt)}`}>{quotaUpdatedLabel(a.quota.updatedAt)}{stale ? ' · 上次数据' : ''}</time> : <span>{refreshing ? '正在获取首次额度' : a.state === 'reauth' ? '重新授权后自动查询' : a.state === 'deleting' ? '账号正在移除' : '等待自动查询，也可手动刷新'}</span>}</div>{cooling && <span>冷却至 {date(a.cooldownUntil)}</span>}{a.quotaError && <span className="warning-text">{a.quotaError}；{a.quota ? '保留上次结果' : '额度未知'}</span>}</div>
            <div className="account-tokens"><span>累计 Token <b>{u?.measuredRequests ? num(u.tokens.totalTokens) : '—'}</b></span><span>输入 <b>{u?.measuredRequests ? num(u.tokens.inputTokens) : '—'}</b></span><span>输出 <b>{u?.measuredRequests ? num(u.tokens.outputTokens) : '—'}</b></span></div>
            <ActionButton kind="start" className="account-enable" disabled={busy || pending || !a.available || transitioning || connection.mode === 'unknown'} onClick={() => void run(() => activate(a.id))}><Icon name="power" />{selected && poolOn ? '当前首选账号' : '使用此账号'}<Icon name="arrow" /></ActionButton>
            <div className="account-actions"><ActionButton kind="reset" disabled={busy || a.state === 'reauth' || a.state === 'deleting'} onClick={() => { setCreditId(''); setResetAccount(a); void loadResets(a.id); }}>重置额度</ActionButton><span className="reset-summary">{resetInfo[a.id]?.loading ? '正在查询重置卡…' : resetInfo[a.id]?.error ? `重置卡查询失败；${resetInfo[a.id].credits ? '保留上次结果' : '数量未知'}，点击重置额度重试` : resetInfo[a.id]?.credits ? `可用重置卡 ${resetInfo[a.id].credits!.available_count} 张` : a.state === 'deleting' ? '请重试移除账号' : a.state === 'reauth' ? '重新授权后查询重置卡' : '正在查询重置卡…'}</span><ActionButton kind="delete" aria-label="移除账号" title="移除账号" disabled={busy} onClick={() => setRemoveId(a.id)}>移除</ActionButton></div></article>;
        })}</div>}
        <div className="footnote">{scheduling.preferredAccountId && <button className="text-button" disabled={busy} onClick={() => void run(async () => { await invoke('select_account', { accountId: null }); schedulingRevision.current++; setSettings(await invoke<Settings>('settings')); }, '已切换为自动优先，已有会话保持原账号')}>切换为自动优先</button>}</div>
      </div></div>}
      {showPool && page === 'service' && <div className="service-grid">
        <section><div className="section-head"><h2>原生生图</h2><span className="badge neutral">桌面验收待完成</span></div>
          <p role="status">{imageCompatibility?.detail || '启动时自动检查客户端兼容性，无需手动修改配置'}</p>
          <p className="subtle">桌面 {imageCompatibility?.client.desktop || '未检测到'} · {imageCompatibility?.client.engine || '请打开 Codex 桌面版'}</p>
          <div className="overview-line"><span>配置条件 <b>{imageCompatibility?.configurationReady ? '满足' : '未启用'}</b></span><span>图片路由 <b>{imageCompatibility?.routesReady ? '已接入' : '服务未启动'}</b></span></div>
          <p>{imageCompatibility?.upstreamImageReceived ? '上游已返回图片；Codex 展示、保存及多轮改图仍待真实验收。' : '真实生图待验收；模型目录连通不代表生图可用。'}</p>
          <p className="hint">生成后继续修改保持原会话账号。取消、断网或超时均不会自动重放；上游任务是否停止无法保证。</p>
        </section>
        <section><div className="section-head"><div><h2>服务设置</h2></div></div><div className="fields"><label>监听端口<input type="number" min="1" max="65535" disabled={connection.mode !== 'off' || busy} value={settings.port} onChange={e => setSettings({ ...settings, port: Number(e.target.value) })} /></label><label>单账号并发<input type="number" min="1" max="16" disabled={connection.mode !== 'off' || busy} value={settings.concurrency} onChange={e => setSettings({ ...settings, concurrency: Number(e.target.value) })} /></label><div className="priority-setting"><span>账号优先规则</span><PrioritySelector value={scheduling.policy} disabled={busy} onChange={changePolicy} /></div></div><button disabled={busy || connection.mode !== 'off'} onClick={() => void run(() => invoke('save_settings', { settings }), '设置已保存')}>保存设置</button></section>
        <PricingCards data={pricing} />
        <LocalProjectsCard disabled={busy || transitioning || !!shutdown || service.activeRequests > 0} />
        <NetworkCards poolRunning={poolOn} onError={message => notify({ text: message, error: true })} onNotice={message => notify({ text: message, error: false })} />
      </div>}
      {showPool && <ActiveRequests requests={service.requests ?? []} accounts={accounts} onChanged={() => { reader.current!.invalidate(); refresh(); }} />}
      <section hidden={page !== 'logs' || !showPool}><div className="section-head"><h2>请求记录</h2><ErrorReport disabled={busy} notify={notify}/></div>
        <RequestRecords accounts={accounts} index={historyIndex} indexError={!!readState['对话统计']?.error} revision={logs} active={page==='logs' && showPool}/>

      </section>
    </main>
    <UpdateDialog controller={updates} />
    {resetAccount && <Modal label="使用重置卡" busy={busy} onClose={() => { setResetAccount(null); setCreditId(''); }}><div className="modal small"><h2>使用重置卡</h2><p>账号：{resetAccount.email}</p><p>使用一张官方赠送卡，恢复该卡覆盖的额度。使用后无法撤销；已有会话保持原账号。</p>{resetInfo[resetAccount.id]?.error && <p role="alert">{resetInfo[resetAccount.id].error}</p>}{resetInfo[resetAccount.id]?.credits ? <><p>可用 {resetInfo[resetAccount.id].credits!.available_count} 张 · 额度不足时可使用，是否符合条件由官方判定。</p><label>选择重置卡<select value={creditId} disabled={busy} onChange={e => setCreditId(e.target.value)}><option value="">请选择</option>{resetInfo[resetAccount.id].credits!.credits.filter(c => c.status === 'available' && c.reset_type === 'codex_rate_limits').map(c => <option key={c.id} value={c.id}>额度重置 · {c.expires_at ? new Date(c.expires_at).toLocaleString('zh-CN', { hour12: false }) + ' 到期' : '未提供到期时间'}</option>)}</select></label></> : <p>重置卡信息待查询</p>}<div className="button-row reset-confirm"><button disabled={busy} onClick={() => { setResetAccount(null); setCreditId(''); }}>取消</button><button disabled={busy} onClick={() => void run(() => loadResets(resetAccount.id))}>刷新查询</button><ActionButton kind="reset" disabled={busy || !creditId || !resetInfo[resetAccount.id]?.credits?.credits.some(c => c.id === creditId && c.status === 'available' && c.reset_type === 'codex_rate_limits')} onClick={() => void run(async () => { const id = resetAccount.id; try { const message = await invoke<string>('consume_reset', { id, creditId }); notify({ text: message, error: false }); setResetAccount(null); setCreditId(''); } finally { await loadResets(id); } })}>确认重置额度</ActionButton></div></div></Modal>}
    {removeId && <Modal label="确认操作" busy={busy} onClose={() => setRemoveId(null)}><div className="modal small"><h2>移除这个账号？</h2><p>删除本工具保存的账号与凭据，不会删除你的 OpenAI 账号。</p><p className="warning-text">移除后，关联的池内旧聊天将无法继续；重新添加同一账号也不会恢复这些会话绑定。</p><p>如果只是暂时不用，请取消移除，改用账号卡上的“暂停账号”。</p><div className="button-row"><button disabled={busy} onClick={() => setRemoveId(null)}>取消</button><ActionButton kind="delete" aria-label="确认移除账号" disabled={busy} onClick={() => void run(async () => { await invoke('remove_account', { id: removeId }); setRemoveId(null); }, '操作已完成')}>确认移除</ActionButton></div></div></Modal>}

  </div>;
}
const widgetView = new URLSearchParams(window.location.search).get('view') === 'widget';
if (widgetView) document.documentElement.dataset.widget = 'true';
createRoot(document.getElementById('root')!).render(widgetView ? <PoolWidget /> : <App />);
