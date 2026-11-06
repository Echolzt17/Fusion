import { useState } from 'react';
import './pool-telemetry.css';
import { RequestRecords, type HistoryIndex } from './RequestRecords';

export type Tokens = { inputTokens: number; outputTokens: number; totalTokens: number; cachedTokens: number | null; reasoningTokens: number | null };
export type WindowQuota = { used_percent?: number; reset_at?: number; limit_window_seconds?: number };
export type Account = { id: string; available: boolean; email: string; state: string; cooldownUntil: number; planLabel: string; quota: { rateLimit?: { primary_window?: WindowQuota; secondary_window?: WindowQuota }; planType?: string; updatedAt: number; subscription?: { expiresAt: number; checkedAt: number | null } | null } | null; quotaError: string | null; quotaRefreshing?: boolean };
export type Usage = { account: string; requests: number; measuredRequests: number; tokens: Tokens; days: { day: number; totalTokens: number }[] };
export type Transport = {requestBytes:number|null;responseBytes:number;outputEventObserved:boolean;toolCallObserved:boolean;reconnectStarted:boolean;reconnectRecovered:boolean;attempts:{elapsedMs:number;result:string;networkErrorKind:string|null;bodyBytesYielded:number;bodyFirstYieldMs:number|null;bodyLastYieldMs:number|null}[]};
export type Log = { id: string; at: number; account: string | null; conversationKey?: string | null; startedAt?: number | null; requestedModel?: string | null; responseModel?: string | null; operation: string; status: string; elapsedMs: number; advice: string; usage?: Tokens | null; diagnostic?: { appVersion: string; upstreamStatus: number | null; responseType: string | null; detectedResponseType?: string | null; failureStage: string | null; networkErrorKind?: string | null; proxySource: string; details?:{origin?:string|null;terminalEvent:string|null;upstreamFailure:string|null}|null; execution?:{transport?:Transport|null;localHttpStatus:number|null;upstreamAttempted:boolean;ingressQueueMs?:number|null;queueMs:number|null}|null } | null };
const number = (n: number) => n.toLocaleString('zh-CN');
const date = (n: number) => new Date(n * 1000).toLocaleString('zh-CN', { hour12: false });
const remaining = (q?: WindowQuota) => typeof q?.used_percent === 'number' && Number.isFinite(q.used_percent) ? Math.round(Math.max(0, Math.min(100, 100 - q.used_percent))) : null;
export function PoolTelemetry({ accounts, usage, currentAccountId, index, selection, onSelectionChange, active = true }: { accounts: Account[]; usage: Usage[]; logs?: Log[]; currentAccountId?: string | null; index: HistoryIndex; selection: string; onSelectionChange: (value: string)=>void; active?: boolean }) {
  const [tab, setTab] = useState<'overview' | 'tokens' | 'requests'>('requests');
  const available = accounts.filter(a => a.available).length;
  const total = usage.reduce((n, u) => n + u.tokens.totalTokens, 0);
  const measured = usage.reduce((n, u) => n + u.measuredRequests, 0);
  const requests = usage.reduce((n, u) => n + u.requests, 0);
  return <div className="pool-telemetry">
    <div className="pool-popover" role="region" aria-label="账号池详情">
      <div className="pool-panel-head"><strong>账号池</strong><span>本机统计</span></div>
      <div className="pool-tabs" role="group" aria-label="详情分类"><button aria-pressed={tab === 'overview'} onClick={() => setTab('overview')}>账号与额度</button><button aria-pressed={tab === 'tokens'} onClick={() => setTab('tokens')}>Token</button><button aria-pressed={tab === 'requests'} onClick={() => setTab('requests')}>对话请求</button></div>
      <div className="pool-panel-body">
        {tab === 'overview' ? <>
          <div className="pool-metrics"><div><span>累计 Token</span><b>{measured ? number(total) : '—'}</b></div><div><span>可用 / 全部账号</span><b>{available} / {accounts.length}</b></div><div><span>返回用量的请求</span><b>{measured} / {requests}</b></div></div>
          <p className="pool-note">仅统计经过账号池的请求；缺失用量不计为零。套餐额度为查询快照，不能换算为 Token 余额。</p>
          {accounts.length === 0 && <p className="pool-empty">尚未添加账号，请在账号控制台登录。</p>}
          {accounts.map((a, i) => { const u = usage.find(u => u.account === a.id); const state = a.state === 'ready' ? (a.cooldownUntil > Date.now() / 1000 ? '冷却中' : a.available ? '可用' : '暂不可调度') : a.state === 'paused' ? '已暂停' : a.state === 'deleting' ? '移除未完成' : '需重新授权'; return <article className="pool-account" key={a.id}>
            <div className="pool-account-title"><strong>账号 {i + 1}{a.id === currentAccountId ? ' · 当前显示账号' : ''}</strong><span>{a.planLabel || '套餐未知'} · {state}</span></div>
            <div className="pool-quota-row">{(['primary_window', 'secondary_window'] as const).map((key, j) => { const q = a.quota?.rateLimit?.[key]; const value = remaining(q); const seconds = q?.limit_window_seconds; return <div key={key}><span>{seconds ? `${seconds >= 86400 ? `${Math.round(seconds / 86400)} 天` : `${Math.round(seconds / 3600)} 小时`}周期` : `周期 ${j + 1}`}<b>{value === null ? '待查询' : `${value}% 剩余`}</b></span><div className="pool-meter"><i style={{ width: `${value ?? 0}%` }} /></div><small>{q?.reset_at ? `${date(q.reset_at)} 重置` : '重置时间未知'}</small></div>; })}</div>
            <p className="pool-note">{a.quota ? `快照 ${date(a.quota.updatedAt)}` : '暂无额度快照'}{a.quotaError ? ' · 最近查询失败，保留上次数据' : ''}</p>
            <div className="pool-token-line">输入 {u?.measuredRequests ? number(u.tokens.inputTokens) : '—'} · 输出 {u?.measuredRequests ? number(u.tokens.outputTokens) : '—'} · 总计 {u?.measuredRequests ? number(u.tokens.totalTokens) : '—'}</div>
          </article>; })}
        </> : tab === 'tokens' ? <><div className="pool-metrics"><div><span>输入 Token</span><b>{measured ? number(usage.reduce((n,u) => n+u.tokens.inputTokens,0)) : '—'}</b></div><div><span>输出 Token</span><b>{measured ? number(usage.reduce((n,u) => n+u.tokens.outputTokens,0)) : '—'}</b></div><div><span>累计 Token</span><b>{measured ? number(total) : '—'}</b></div></div>{accounts.map((a,i) => { const u=usage.find(u=>u.account===a.id); return <article className="pool-account" key={a.id}><strong>账号 {i+1}</strong><p className="pool-token-line">输入 {u?.measuredRequests ? number(u.tokens.inputTokens) : '—'} · 输出 {u?.measuredRequests ? number(u.tokens.outputTokens) : '—'} · 总计 {u?.measuredRequests ? number(u.tokens.totalTokens) : '—'}</p><p className="pool-token-line">缓存 {u?.tokens.cachedTokens == null ? '未提供' : number(u.tokens.cachedTokens)} · 推理 {u?.tokens.reasoningTokens == null ? '未提供' : number(u.tokens.reasoningTokens)}</p></article>; })}<p className="pool-note">缓存包含在输入中，推理包含在输出中；缺失明细不计为零。仅统计经过账号池的已上报用量。</p></> : null}
        <div hidden={tab!=='requests'}><RequestRecords accounts={accounts} index={index} selection={selection} onSelectionChange={onSelectionChange} active={active && tab==='requests'}/></div>
      </div>
    </div>
  </div>;
}

export function LogDiagnostic({ log }: { log: Log }) {
  const d = log.diagnostic;
  if (!d) return <p className="subtle">历史日志 · 未采集版本与上游分类，无法确认原因</p>;
  const mime: Record<string, string> = { sse: 'SSE 事件流', html: 'HTML 网页', json: 'JSON', missing: '未提供 Content-Type', other: '其他' };
  const responseType = d.detectedResponseType
    ? `${mime[d.detectedResponseType] || '未知'}（内容已识别）`
    : d.responseType && d.responseType !== 'missing' && d.responseType !== 'other'
      ? `${mime[d.responseType] || '未知'}（响应头声明）`
      : d.upstreamStatus == null ? '未收到响应' : '未识别';
  const stage: Record<string, string> = { ingress_queue:'入口排队',upload:'读取请求正文',request_validation:'请求格式校验',queue: '等待账号名额', proxy_resolution: '读取代理配置', connect: '建立连接', response_headers: '等待响应', response_read: '读取响应', event_validation: '事件校验', upstream_rejected: '上游拒绝', cancelled: '客户端取消', service_stopped: '服务停止', unknown: '未确认' };
  const terminal:Record<string,string>={completed:'完成',failed:'失败',incomplete:'未完成',error:'错误事件'};
  const cause:Record<string,string>={authorization:'授权被拒绝',rate_limit:'限流或额度限制',server:'上游服务错误',max_output_tokens:'达到输出 Token 限制',content_filter:'内容过滤',unknown:'未确认'};
  const network: Record<string, string> = { dns: '名称解析失败', connection_refused: '连接被拒绝', connection_reset: '连接被重置', connection_aborted: '连接被中止', connection_closed: '连接已关闭', connect: '建立连接失败，具体原因未确认', tls: 'TLS 校验或握手失败', proxy_authentication: '代理要求身份验证', proxy_tunnel: '代理隧道建立失败', timeout: '网络等待超时', http_protocol: 'HTTP 传输协议异常', body_read: '响应体传输读取失败', unexpected_eof: '传输数据尚未完整接收即结束', stream_ended: '响应流提前结束，未收到完成事件', unknown: '网络错误，具体原因未确认' };
  const proxy: Record<string, string> = { environment_configured: '环境变量', windows_static_configured: 'Windows 静态代理', windows_automatic_configured: 'Windows 自动代理', loopback_direct: '本机直连', none_configured: '未配置代理（历史记录）', unknown: '未确认' };
  return <div className="subtle"><dl><dt>故障类别</dt><dd>{({local:"本机处理",transport:"网络传输",upstream_service:"上游服务",account:"账号授权或额度",unknown:"未确认"} as Record<string,string>)[d.details?.origin || "unknown"]}</dd><dt>失败阶段</dt><dd>{d.failureStage ? stage[d.failureStage] || '未确认' : '未提供'}</dd><dt>网络错误分类</dt><dd>{d.networkErrorKind ? network[d.networkErrorKind] || '未确认' : '未提供'}</dd><dt>本机 HTTP</dt><dd>{d.execution?.localHttpStatus ?? '未采集'}</dd><dt>上游尝试</dt><dd>{d.execution ? d.execution.upstreamAttempted?'已尝试，执行结果以事件为准':'未发送':'未采集'}</dd><dt>上游 HTTP</dt><dd>{d.upstreamStatus ?? '未收到'}</dd><dt>终止事件</dt><dd>{d.details?.terminalEvent?terminal[d.details.terminalEvent]||'未确认':'未采集'}</dd><dt>上游失败原因</dt><dd>{d.details?.upstreamFailure?cause[d.details.upstreamFailure]||'未确认':'未采集'}</dd><dt>响应类型</dt><dd>{responseType}</dd><dt>响应头声明</dt><dd>{d.responseType ? mime[d.responseType] || '未知' : d.upstreamStatus == null ? '未收到' : '未采集'}</dd><dt>Fusion 版本</dt><dd>{d.appVersion || '未知'}</dd><dt>代理配置来源</dt><dd>{proxy[d.proxySource] || '未确认'}</dd></dl>{d.execution?.transport && <TransportDetails value={d.execution.transport} />}<p className="subtle">响应类型优先显示内容识别结果，响应头声明单独保留。代理来源不代表已证实其远端出口。旧记录不补推内容类型或网络错误分类。</p></div>;
}

function TransportDetails({value:t}:{value:Transport}) {
  const results:Record<string,string>={headers:'收到响应头',connect_failure:'发送前建连失败',send_failure:'发送阶段失败',cancelled:'已取消',deadline:'达到截止时间'};
  return <details><summary>传输过程 · {t.attempts.length} 次尝试{t.reconnectStarted ? t.reconnectRecovered ? ' · 重连后收到响应' : ' · 已尝试重连' : ''}</summary><p>请求 {t.requestBytes ?? '未知'} 字节 · 已收到 {t.responseBytes} 字节；观察到输出事件：{t.outputEventObserved?'是':'否'}，工具调用事件：{t.toolCallObserved?'是':'否'}。</p><ol>{t.attempts.map((a,i)=><li key={i}>{results[a.result] || '未确认'} · 发送至响应头阶段 {a.elapsedMs} ms · 正文已交给 HTTP 层 {a.bodyBytesYielded} 字节{a.bodyLastYieldMs !== null ? `，最后进度 ${a.bodyLastYieldMs} ms` : ''}</li>)}</ol><p>正文交付是本机观测，不代表远端收齐或已经执行；尝试耗时包含建连、发送与等待响应。</p></details>;
}
