import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Modal } from './Modal';
import { RecordSelector } from './RecordSelector';
import type { ApiProtocol, ApiRequestLog, ApiSource, CheckResult, ClientId, ClientStatus, SourceInput, WorkspacePage } from './api-types';
import './client-workspace.css';

export type { ClientId, ClientStatus } from './api-types';
export type WorkspaceView = ClientId | 'api';

const clients: Record<ClientId, string> = { codex: 'Codex', claudeCode: 'Claude Code' };
const protocols: Record<ApiProtocol, string> = { responses: 'OpenAI Responses', messages: 'Anthropic Messages', chatCompletions: 'OpenAI Chat Completions' };
const states: Record<ClientStatus['mode'], string> = { off: 'API 已关闭', starting: '正在启动 API', running: 'API 运行中', stopping: '正在关闭 API', recoveryRequired: '接入需要恢复', failed: '操作未完成' };
const phases: Record<string, string> = {
  checking: '正在检查来源与客户端', configuring: '正在配置客户端', draining: '等待在途请求完成',
  restoring: '正在恢复原接入', quitting: '正在正常退出客户端', terminating: '正在核验并结束残留进程',
  launching: '正在打开客户端', verifying: '正在核验接入', starting: '正在准备连接', stopping: '正在关闭连接',
  stoppingClient: '正在正常关闭当前客户端', verifyingRetries: '正在隔离检查客户端重试行为',
};

const compatible = (client: ClientId, protocol: ApiProtocol) => client === 'codex' ? protocol === 'responses' : protocol === 'messages';
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const count = (value: unknown) => finite(value) ? value.toLocaleString('zh-CN') : '未采集';
const errorText = (error: unknown, secret?: string) => {
  let value = String(error);
  if (secret) value = value.split(secret).join('[已隐藏]');
  return value.slice(0, 1200);
};
const time = (value: number | undefined | null) => finite(value) && value > 0 ? new Date(value < 1e12 ? value * 1000 : value).toLocaleString('zh-CN', { hour12: false }) : '时间未记录';

export function WorkspaceSwitcher({ value, onChange }: { value: WorkspaceView; onChange: (value: WorkspaceView) => void }) {
  // Li-Deheng/shy-moth-10 (MIT): three overlapping chevrons spread on hover.
  // Source and full license: THIRD_PARTY_NOTICES.md.
  const arrowPath = 'M0.154393339,3.89485454 L3.97631488,0.139296592 C4.17083111,-0.0518420739 4.48263286,-0.0518571125 4.67716753,0.139262789 L25.6916134,20.7848311 C26.0855801,21.1718824 26.0911863,21.8050225 25.704135,22.1989893 C25.7000188,22.2031791 25.6958657,22.2073326 25.6916762,22.2114492 L4.67709797,42.8607841 C4.48259567,43.0519059 4.17082418,43.0519358 3.97628526,42.8608513 L0.154518591,39.1069479 C-0.0424848215,38.9134427 -0.0453206733,38.5968729 0.148184538,38.3998695 C0.150289256,38.3977268 0.152413239,38.395603 0.154556228,38.3934985 L16.9937789,21.8567812 C17.1908028,21.6632968 17.193672,21.3467273 17.0001876,21.1497035 C16.9980647,21.1475418 16.9959223,21.1453995 16.9937605,21.1432767 L0.15452076,4.60825197 C-0.0425130651,4.41477773 -0.0453986756,4.09820839 0.148075568,3.90117456 C0.150162624,3.89904911 0.152268631,3.89694235 0.154393339,3.89485454 Z';
  return <div className="cw-workspace-picker">
    <RecordSelector label="工作区" value={value} options={[
      { value: 'codex', label: 'Codex' },
      { value: 'claudeCode', label: 'Claude Code' },
      { value: 'api', label: 'API' },
    ]} onChange={next => onChange(next as WorkspaceView)} closeOnSelect align="start" triggerIcon={<span className="cw-switch-arrow" aria-hidden="true">
      <svg width="66" height="43" viewBox="0 0 66 43" focusable="false">
        <g className="cw-switch-arrow-one"><path d={arrowPath} transform="translate(40 0)" /></g>
        <g className="cw-switch-arrow-two"><path d={arrowPath} transform="translate(20 0)" /></g>
        <g className="cw-switch-arrow-three"><path d={arrowPath} /></g>
      </svg>
    </span>} />
  </div>;
}

function CheckBadge({ label, result }: { label: string; result?: CheckResult }) {
  const names: Record<CheckResult['status'], string> = { unknown: '待验证', checking: '检查中', passed: '已通过', failed: '未通过' };
  const state = result?.status ?? 'unknown';
  return <span className={`cw-check cw-check-${state}`} title={result?.message || `${label}尚未验证`}>
    {label}<b>{names[state] ?? '待验证'}</b>
  </span>;
}

type EditorProps = { source: ApiSource | null; client: ClientId; onClose: () => void; onSave: (input: SourceInput) => Promise<void> };
function SourceEditor({ source, client, onClose, onSave }: EditorProps) {
  const prefix = useId();
  const nameInput = useRef<HTMLInputElement>(null);
  useEffect(() => { nameInput.current?.focus(); }, []);
  const [name, setName] = useState(source?.name ?? '');
  const [protocol, setProtocol] = useState<ApiProtocol>(source?.protocol ?? (client === 'codex' ? 'responses' : 'messages'));
  const [baseUrl, setBaseUrl] = useState(source?.baseUrl ?? '');
  const [model, setModel] = useState(source?.model ?? '');
  const [network, setNetwork] = useState(source?.network ?? 'systemProxy');
  const [enabled, setEnabled] = useState(source?.enabled ?? true);
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function save(event: React.FormEvent) {
    event.preventDefault();
    if (busy) return;
    if (!name.trim() || !baseUrl.trim() || !model.trim() || (!source?.hasKey && !apiKey.trim())) {
      setError('请填写名称、Base URL、模型与 API Key。'); return;
    }
    try {
      const url = new URL(baseUrl.trim());
      if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error();
    } catch { setError('请输入有效的 HTTP(S) Base URL，不包含账号密码、查询参数或片段。'); return; }
    setBusy(true); setError('');
    try {
      await onSave({ id: source?.id ?? null, name: name.trim(), protocol, baseUrl: baseUrl.trim(), model: model.trim(), network, enabled, apiKey: apiKey.trim() || null });
      setApiKey(''); onClose();
    } catch (e) { setError(errorText(e, apiKey)); setApiKey(''); }
    finally { setBusy(false); }
  }
  return <Modal label={source ? '编辑 API 来源' : '添加 API 来源'} busy={busy} onClose={onClose}>
    <form className="cw-drawer" onSubmit={event => void save(event)}>
      <div className="cw-drawer-heading"><div><span className="cw-eyebrow">API SOURCE</span><h2>{source ? '编辑来源' : '添加来源'}</h2></div><button type="button" aria-label="关闭来源编辑" disabled={busy} onClick={onClose}>×</button></div>
      <p className="cw-description">填写官方 API 或你选择的中转服务。保存后仍需明确启动，现有连接不会随表单更改。</p>
      <label htmlFor={prefix + '-name'}>来源名称<input ref={nameInput} id={prefix + '-name'} required maxLength={80} value={name} onChange={e => setName(e.target.value)} placeholder="例如：工作用 API" disabled={busy} /></label>
      <label htmlFor={prefix + '-protocol'}>接口协议<select id={prefix + '-protocol'} value={protocol} onChange={e => setProtocol(e.target.value as ApiProtocol)} disabled={busy}>
        {(Object.keys(protocols) as ApiProtocol[]).map(value => <option key={value} value={value}>{protocols[value]}</option>)}
      </select></label>
      <label htmlFor={prefix + '-url'}>Base URL<input id={prefix + '-url'} type="url" autoCapitalize="off" autoComplete="off" spellCheck={false} required value={baseUrl} onChange={e => setBaseUrl(e.target.value)} placeholder={protocol === 'messages' ? 'https://api.example.com' : 'https://api.example.com/v1'} disabled={busy} /></label>
      <p className="cw-field-note">填写服务提供的接口根地址，保留其要求的路径；不要填完整的请求地址。</p>
      <label htmlFor={prefix + '-model'}>模型名称<input id={prefix + '-model'} autoComplete="off" spellCheck={false} required value={model} onChange={e => setModel(e.target.value)} placeholder="按服务提供的模型 ID 填写" disabled={busy} /></label>
      <label htmlFor={prefix + '-key'}>API Key<input id={prefix + '-key'} type="password" autoComplete="new-password" spellCheck={false} required={!source?.hasKey} value={apiKey} onChange={e => setApiKey(e.target.value)} placeholder={source?.hasKey ? '已保存密钥，留空保留' : '输入服务颁发的 API Key'} disabled={busy} /></label>
      <p className="cw-field-note">密钥只提交给本机后端，不在来源卡片或日志中展示。{source?.hasKey ? '编辑时留空保留现有密钥。' : ''}</p>
      <label htmlFor={prefix + '-network'}>网络连接<select id={prefix + '-network'} value={network} onChange={e => setNetwork(e.target.value as SourceInput['network'])} disabled={busy}><option value="systemProxy">跟随系统代理</option><option value="direct">直接连接</option></select></label>
      <label className="cw-checkbox"><input type="checkbox" checked={enabled} onChange={e => setEnabled(e.target.checked)} disabled={busy} />启用此来源</label>
      {!compatible(client, protocol) && <p className="cw-callout">{clients[client]} 首版使用 {client === 'codex' ? 'Responses' : 'Messages'} 协议。此配置可保存和检查，当前客户端不能直接启动它。</p>}
      {source && <p className="cw-callout">请先关闭使用此来源的连接再编辑。修改协议、地址、模型、网络方式或密钥会创建新修订，旧聊天不能通过新修订继续；需要保留旧聊天时，请新增来源并保留原配置。名称和启用状态不改变修订。</p>}
      {error && <p className="cw-error" role="alert">{error}</p>}
      <div className="cw-drawer-footer"><button type="button" onClick={onClose} disabled={busy}>取消</button><button className="primary" type="submit" disabled={busy}>{busy ? '正在保存…' : '保存来源'}</button></div>
    </form>
  </Modal>;
}

export type ApiWorkspaceProps = {
  client: ClientId;
  onClientChange: (value: ClientId) => void;
  page: WorkspacePage;
  poolMode?: boolean;
  poolBusy?: boolean;
  onSwitchToPool?: () => void;
  onStatusChange?: (statuses: ClientStatus[]) => void;
};

export function ApiWorkspace({ client, onClientChange, page, poolMode = false, poolBusy = false, onSwitchToPool, onStatusChange }: ApiWorkspaceProps) {
  const [sources, setSources] = useState<ApiSource[]>([]);
  const [statuses, setStatuses] = useState<ClientStatus[]>([]);
  const [logs, setLogs] = useState<Partial<Record<ClientId, ApiRequestLog[]>>>({});
  const [candidates, setCandidates] = useState<Partial<Record<ClientId, string>>>({});
  const [readErrors, setReadErrors] = useState<Record<string, string>>({});
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [notices, setNotices] = useState<Partial<Record<ClientId, { text: string; error: boolean }>>>({});
  const [editor, setEditor] = useState<{ source: ApiSource | null } | null>(null);
  const [removing, setRemoving] = useState<ApiSource | null>(null);
  const [lifecycleIntent, setLifecycleIntent] = useState<Partial<Record<ClientId, 'start' | 'stop'>>>({});
  const requestVersion = useRef(0);
  const mounted = useRef(true);
  const operations = useRef(new Set<string>());
  const statusCallback = useRef(onStatusChange); statusCallback.current = onStatusChange;

  const refresh = useCallback(async () => {
    const version = ++requestVersion.current;
    const results = await Promise.allSettled([
      invoke<ApiSource[]>('api_sources'), invoke<ClientStatus[]>('api_client_status'),
      ...(page === 'logs' ? [invoke<ApiRequestLog[]>('api_request_logs', { client })] : []),
    ]);
    if (!mounted.current || version !== requestVersion.current) return;
    const nextErrors: Record<string, string> = {};
    const [sourceResult, statusResult, logResult] = results;
    if (sourceResult.status === 'fulfilled' && Array.isArray(sourceResult.value)) { setSources(sourceResult.value as ApiSource[]); setLoaded(true); }
    else nextErrors.sources = sourceResult.status === 'rejected' ? errorText(sourceResult.reason) : '来源数据格式无效';
    if (statusResult.status === 'fulfilled' && Array.isArray(statusResult.value)) { const value = statusResult.value as ClientStatus[]; setStatuses(value); statusCallback.current?.(value); }
    else nextErrors.status = statusResult.status === 'rejected' ? errorText(statusResult.reason) : '客户端状态数据格式无效';
    if (logResult?.status === 'fulfilled' && Array.isArray(logResult.value)) setLogs(previous => ({ ...previous, [client]: logResult.value as ApiRequestLog[] }));
    else if (logResult) nextErrors.logs = logResult.status === 'rejected' ? errorText(logResult.reason) : '请求记录格式无效';
    setReadErrors(nextErrors);
  }, [client, page]);
  const latestRefresh = useRef(refresh); latestRefresh.current = refresh;

  useEffect(() => { mounted.current = true; return () => { mounted.current = false; requestVersion.current++; }; }, []);
  useEffect(() => {
    void refresh(); const timer = setInterval(() => void refresh(), 2000);
    return () => { clearInterval(timer); requestVersion.current++; };
  }, [refresh]);
  useEffect(() => { setEditor(null); setRemoving(null); }, [client]);

  const status = statuses.find(value => value.client === client);
  const current = sources.find(value => value.id === status?.sourceId);
  const candidate = sources.find(value => value.id === candidates[client]);
  const transition = status?.mode === 'starting' || status?.mode === 'stopping';
  const lifecycleBusy = !!busy['client:' + client] || transition || (client === 'codex' && poolBusy);
  const needsRecovery = status?.mode === 'recoveryRequired' || status?.mode === 'failed';
  const connected = !!status && status.mode !== 'off';
  const selectedIsCurrent = !!candidate && candidate.id === status?.sourceId && status.mode === 'running' && status.sourceRevision === candidate.revision;
  const canStart = !!candidate && candidate.enabled && candidate.hasKey && compatible(client, candidate.protocol) && !lifecycleBusy && !needsRecovery && !readErrors.sources && !readErrors.status && !!status && !selectedIsCurrent;
  const notice = notices[client];

  async function run(key: string, task: () => Promise<unknown>, success?: string) {
    if (operations.current.has(key)) return;
    const targetClient = client;
    operations.current.add(key); setBusy(previous => ({ ...previous, [key]: true }));
    setNotices(previous => ({ ...previous, [targetClient]: undefined }));
    try {
      await task();
      if (mounted.current && success) setNotices(previous => ({ ...previous, [targetClient]: { text: success, error: false } }));
    } catch (e) {
      if (mounted.current) setNotices(previous => ({ ...previous, [targetClient]: { text: errorText(e), error: true } }));
    } finally {
      operations.current.delete(key);
      if (mounted.current) { setBusy(previous => ({ ...previous, [key]: false })); await latestRefresh.current(); }
    }
  }
  async function saveSource(input: SourceInput) {
    await invoke<ApiSource>('save_api_source', { input });
    await refresh();
  }
  function choose(source: ApiSource) { setCandidates(previous => ({ ...previous, [client]: source.id })); }
  function start() {
    if (!candidate || !canStart) return;
    const targetClient = client;
    setLifecycleIntent(previous => ({ ...previous, [targetClient]: 'start' }));
    void run('client:' + client, () => invoke('start_api_connection', { client, sourceId: candidate.id }), '连接操作已完成；请在客户端新建聊天，真实客户端能力仍待验收。')
      .finally(() => { if (mounted.current) setLifecycleIntent(previous => ({ ...previous, [targetClient]: undefined })); });
  }
  function stop() {
    const targetClient = client;
    setLifecycleIntent(previous => ({ ...previous, [targetClient]: 'stop' }));
    void run('client:' + client, () => invoke('stop_api_connection', { client }), 'API 连接已关闭，原接入已恢复。')
      .finally(() => { if (mounted.current) setLifecycleIntent(previous => ({ ...previous, [targetClient]: undefined })); });
  }
  function check(source: ApiSource, actual: boolean) {
    void run('source:' + source.id, () => invoke('check_api_source', { id: source.id, actual }), actual ? '少量实际请求测试已完成；结果见来源卡片，不代表工具或真实客户端验收通过。' : '基础检查已完成；结果见来源卡片。');
  }
  function toggle(source: ApiSource) {
    void run('source:' + source.id, () => invoke('save_api_source', { input: { id: source.id, name: source.name, protocol: source.protocol, baseUrl: source.baseUrl, model: source.model, network: source.network, enabled: !source.enabled, apiKey: null } }), source.enabled ? '来源已禁用。' : '来源已启用，仍需明确启动。');
  }
  const startLabel = lifecycleBusy ? '正在处理连接…' : poolMode && client === 'codex' ? '从账号池切换到此来源' : selectedIsCurrent ? '当前来源已运行' : candidate?.id === status?.sourceId && status?.mode === 'running' ? '重新应用选中来源' : connected ? '切换到选中来源' : '启动选中来源';

  return <div className="cw-workspace" data-client={client}>
    <div className="cw-api-toolbar"><label>目标客户端<select aria-label="目标客户端" value={client} onChange={event => onClientChange(event.target.value as ClientId)}>
      {(Object.keys(clients) as ClientId[]).map(value => <option key={value} value={value}>{clients[value]}</option>)}
    </select></label></div>
    <div className="cw-running-panel">
      <div className="cw-running-copy"><span className="cw-eyebrow">{clients[client].toUpperCase()} / API</span>
        <div className="cw-running-title"><span className={'cw-status-dot ' + (transition || lifecycleIntent[client] ? 'is-busy' : status?.mode === 'running' ? 'is-running' : '')} /><h2>{readErrors.status ? '运行状态读取失败' : lifecycleIntent[client] ? lifecycleIntent[client] === 'start' ? '正在启动 API' : '正在关闭 API' : status ? states[status.mode] : '正在读取运行状态'}</h2></div>
        <p>{status?.sourceId ? `当前运行：${status.sourceName || current?.name || '来源信息不可用'}` : client === 'codex' && poolMode ? 'Codex 订阅账号池仍在运行' : '当前没有由 Fusion 托管的 API 连接'}</p>
        {current && (status?.sourceRevision == null || current.revision === status.sourceRevision) && <span className="cw-running-model">{current.model} · {protocols[current.protocol]}{status?.sourceRevision == null ? '（保存的配置）' : ''}</span>}
        <div className="cw-status-details"><span>在途请求 <b>{status ? count(status.activeRequests) : '未知'}</b></span><span>真实客户端验收 <b>待验收</b></span></div>
      </div>
      <div className="cw-running-actions">
        <button className="primary" onClick={start} disabled={!canStart}>{startLabel}</button>
        {connected && <button onClick={stop} disabled={lifecycleBusy || !!readErrors.status}>{needsRecovery ? '重试恢复并关闭' : '关闭 API 连接'}</button>}
        {!candidate && <span className="cw-field-note">先选择一个候选来源</span>}
      </div>
    </div>
    {(transition || busy['client:' + client]) && <p className="cw-progress" role="status"><span className="cw-progress-line" />{status?.phase ? phases[status.phase] ?? '正在处理客户端连接' : '正在处理客户端连接'}{status && status.activeRequests > 0 ? `（${status.activeRequests} 个在途请求）` : ''}</p>}
    {needsRecovery && <div className="cw-callout cw-danger" role="alert"><strong>上次操作尚未完成</strong><p>{status?.error || '恢复记录仍保留。请重试恢复并关闭，完成后再切换来源。'}</p></div>}
    {notice && <p className={notice.error ? 'cw-error' : 'cw-notice'} role={notice.error ? 'alert' : 'status'}>{notice.text}</p>}
    {status?.historyIncomplete && <p className="cw-callout" role="status">部分 API 请求记录未能保存，当前记录可能不完整。</p>}
    {Object.entries(readErrors).map(([key, error]) => <div key={key} className="cw-error" role="alert">{({ sources: '来源', status: '运行状态', logs: '请求记录' } as Record<string, string>)[key]}读取失败：{error}<button onClick={() => void refresh()}>重试读取</button></div>)}
    <p className="cw-continuity">选中卡片只改变候选来源，点击启动或切换后才生效。切换会等待在途请求并按需重开当前客户端；旧会话仍需切回原来源，账号与加密上下文不会自动迁移。</p>
    {client === 'claudeCode' && <p className="cw-callout">首版受管终端使用原生 claude.exe，为控制自动重试，不加载用户和项目 settings 中的 hooks、插件及权限配置。原有文件和官方登录保持不变；如需这些自定义设置，请使用官方客户端路径。</p>}

    {page === 'logs' ? <div className="cw-section">
      <div className="cw-heading"><div><h2>{clients[client]} API 请求记录</h2><p>只显示当前客户端经 Fusion 处理的请求；订阅账号池记录在订阅视图中。</p></div><button onClick={() => void refresh()}>刷新记录</button></div>
      <div className="cw-log-list">{logs[client]?.length ? logs[client]!.map(log => <article className="cw-log" key={log.id}>
        <div className="cw-log-top"><strong>{({ success: '成功', completed: '完成', failed: '失败', cancelled: '已取消', stream_interrupted: '流中断', upstream_error: '上游失败', upstream_transport: '连接失败', upstream_http: '上游拒绝', context_storage: '上下文绑定保存失败', context_binding: '上下文不匹配' } as Record<string, string>)[log.status] ?? '请求未完成'}</strong><span>{log.model || '请求模型未记录'}{log.responseModel && log.responseModel !== log.model ? ` → 返回 ${log.responseModel}` : ''}</span><time>{time(log.at)}</time></div>
        <p>{sources.find(source => source.id === log.sourceId)?.name || '来源已移除或未记录'} · {log.operation} · {finite(log.elapsedMs) ? `${(log.elapsedMs / 1000).toFixed(1)} 秒` : '耗时未记录'}</p>
        <div className="cw-log-usage"><span>输入 {log.usagePartial ? '未完整采集' : count(log.usage?.inputTokens)}</span><span>输出 {log.usagePartial ? '未完整采集' : count(log.usage?.outputTokens)}</span><span>总 Token {log.usagePartial ? '未完整采集' : count(log.usage?.totalTokens)}</span></div>
        {log.usagePartial && <p className="cw-field-note">用量未完整采集，暂不显示 Token 合计。</p>}
        <small>请求编号 {log.id}</small>
      </article>) : <p className="cw-empty">{readErrors.logs ? '暂时无法读取请求记录。' : logs[client] ? '暂无已记录的 API 请求。' : '正在读取请求记录…'}</p>}</div>
    </div> : <>
      {page === 'service' && <div className="cw-section cw-connection-details"><h2>当前连接</h2><dl>
        <div><dt>客户端</dt><dd>{clients[client]}</dd></div><div><dt>当前来源</dt><dd>{current?.name || '尚未启动'}</dd></div>
        <div><dt>网络方式</dt><dd>{current ? current.network === 'direct' ? '直接连接' : '跟随系统代理' : '以所选来源为准'}</dd></div>
        <div><dt>本机入口</dt><dd>{status?.baseUrl || '尚未提供'}</dd></div>
      </dl><p className="cw-description">只影响此客户端。关闭 API 连接会恢复启用前的接入；真实客户端展示、工具调用及长任务应分别验收。</p>
        {client === 'codex' && onSwitchToPool && <button onClick={onSwitchToPool}>查看订阅账号池</button>}
      </div>}
      <div className="cw-section">
        <div className="cw-heading"><div><h2>API 与中转来源 <span className="cw-total">{sources.length}</span></h2><p>来源余额未知；不同服务的余额与订阅额度不合并。</p></div><button className="primary" onClick={() => setEditor({ source: null })}>＋ 添加来源</button></div>
        <div className="cw-source-grid">{sources.map(source => {
          const selected = source.id === candidate?.id;
          const running = status?.sourceId === source.id && status.mode !== 'off';
          const inUse = statuses.some(value => value.sourceId === source.id && value.mode !== 'off');
          const checking = !!busy['source:' + source.id];
          const supported = compatible(client, source.protocol);
          return <article className={'cw-source-card' + (selected ? ' is-selected' : '') + (!source.enabled ? ' is-disabled' : '')} key={source.id} aria-label={`来源 ${source.name}`}>
            <div className="cw-card-heading"><div><h3>{source.name}</h3><span className="cw-protocol">{protocols[source.protocol]}</span></div><span className="cw-source-state">{!source.enabled ? '已禁用' : running ? '当前运行' : selected ? '候选选中' : supported ? '可选来源' : '协议不匹配'}</span></div>
            <p className="cw-source-url" title={source.baseUrl}>{source.baseUrl}</p>
            <div className="cw-source-info"><span>模型 <b>{source.model}</b></span><span>余额 <b>未知</b></span><span>网络 <b>{source.network === 'direct' ? '直连' : '系统代理'}</b></span><span>密钥 <b>{source.hasKey ? '已保存' : '未保存'}</b></span></div>
            <div className="cw-checks"><CheckBadge label="基础检查" result={source.verification?.basic} /><CheckBadge label="实际请求" result={source.verification?.test} /><CheckBadge label="流式" result={source.verification?.stream} /><CheckBadge label="工具调用" result={source.verification?.tools} /><span className="cw-check">客户端<b>待验收</b></span></div>
            {running && status.sourceRevision != null && status.sourceRevision !== source.revision && <p className="cw-callout">运行配置与保存配置不同，点击切换后应用新配置。</p>}
            {selected && <p className="cw-selected-note">候选已选中{running ? ' · 此来源同时正在运行' : ' · 尚未启动'}</p>}
            <button className="cw-choose" aria-pressed={selected} onClick={() => choose(source)} disabled={!source.enabled || !supported}>{selected ? '已选为候选' : '选择此来源'}</button>
            <div className="cw-card-actions"><button onClick={() => setEditor({ source })} disabled={checking}>编辑</button><button onClick={() => check(source, false)} disabled={checking || !source.hasKey}>{checking ? '检查中…' : '基础检查'}</button><button onClick={() => toggle(source)} disabled={checking || inUse} title={inUse ? '请先关闭使用此来源的连接' : undefined}>{source.enabled ? '禁用' : '启用'}</button><button className="cw-remove" onClick={() => { setNotices(previous => ({ ...previous, [client]: undefined })); setRemoving(source); }} disabled={checking || inUse} title={inUse ? '请先关闭使用此来源的连接' : undefined}>移除</button></div>
            <button className="cw-actual-test" onClick={() => check(source, true)} disabled={checking || !source.hasKey || !source.enabled}>少量实际测试（可能计费）</button>
            {source.verification?.basic?.message && <p className="cw-result-note">基础：{source.verification.basic.message}</p>}
            {source.verification?.test?.message && <p className="cw-result-note">请求：{source.verification.test.message}</p>}
          </article>;
        })}</div>
        {!sources.length && <div className="cw-empty"><strong>{loaded ? '还没有 API 来源' : '正在读取 API 来源…'}</strong><p>添加来源后先检查，再选择并明确启动。保存与选中都不会切换客户端。</p></div>}
        <p className="cw-description">基础检查不代表模型推理成功。少量实际测试会向该来源发送测试请求，可能消耗额度；不会因此验证流式、工具调用或真实客户端。</p>
      </div>
    </>}
    {editor && <SourceEditor source={editor.source} client={client} onClose={() => setEditor(null)} onSave={saveSource} />}
    {removing && <Modal label="移除 API 来源" busy={!!busy['source:' + removing.id]} onClose={() => setRemoving(null)}><div className="modal small cw-remove-dialog"><h2>移除 {removing.name}？</h2><p>来源配置与保管的密钥将被移除。使用该来源的旧会话可能无法继续；临时停用可使用禁用。</p>{notice?.error && <p className="cw-error" role="alert">{notice.text}</p>}<div className="button-row"><button onClick={() => setRemoving(null)} disabled={!!busy['source:' + removing.id]}>取消</button><button className="cw-destructive" disabled={!!busy['source:' + removing.id]} onClick={() => {
      const source = removing;
      void run('source:' + source.id, async () => { await invoke('remove_api_source', { id: source.id }); setRemoving(null); setCandidates(previous => Object.fromEntries(Object.entries(previous).filter(([, id]) => id !== source.id))); }, '来源已移除。');
    }}>确认移除</button></div></div></Modal>}
  </div>;
}
