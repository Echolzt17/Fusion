import type { Log } from './PoolTelemetry';

export type NoticeLevel = 'info' | 'success' | 'warning' | 'error';
export type NoticeInput = { title: string; message: string; level: NoticeLevel };
export type Notification = NoticeInput & { id: number; at: number; lastAt: number; read: boolean; count: number; requestId?: string; conversation?: string; operation?: string; requests?: { id:string; at:number; status:string; conversation?:string|null; operation:string }[]; omittedRequests?:number };
const WINDOW_MS = 5 * 60 * 1000;
const statusNames: Record<string, string> = { ingress_busy:'本机等待队列已满',ingress_timeout:'入口排队超时',upload_idle_timeout:'等待上传数据超时',upload_timeout:'实际上传超时',upload_wait_timeout:'正文处理等待超时',body_read_error:'正文传输失败',json_structure_limit:'请求结构超过限制',invalid_request:'请求格式无效',request_too_large:'请求超过大小限制',proxy_configuration:'代理配置不可用', upstream_header_timeout: '等待上游响应超时', upstream_read_timeout: '读取上游响应超时', upstream_empty_response: '上游返回空响应', invalid_event_stream: '上游事件未完成', upstream_response_too_large: '上游响应超过限制', upstream_forbidden: '上游访问被拒绝', reauthorization_required: '账号需重新授权', upstream_bad_request: '上游不接受请求', response_incomplete: '上游响应未完成', invalid_image_response: '上游图片响应异常', unexpected_content_type: '上游响应格式异常', network_error: '上游连接失败', read_error: '上游读取失败', stream_interrupted: '响应流中断', stream_invalid: '上游事件格式异常', upstream_rate_limited: '账号限流', account_unavailable: '账号不可用', authorization_unavailable: '账号授权不可用', response_failed: '上游响应失败', continuity_unavailable: '会话上下文不可用', upstream_error: '上游服务异常', service_stopped: '服务已停止', cancelled: '请求已取消，执行状态未确认' };
export const requestStatusName = (code: string) => statusNames[code] || `请求异常 · ${code}`;


// In-memory UI inbox. Only the existing safe log DTO is consumed; nothing is written to browser storage.
export function createNotificationStore(changed: (items: Notification[]) => void) {
  let items: Notification[] = [];
  let nextId = 1;
  const startedAt = Math.floor(Date.now() / 1000) * 1000; // Logs have whole-second precision.
  const active = new Map<string, string>();
  const seenLogs = new Set<string>();
  const groups = new Map<string, number>();
  const publish = () => changed([...items].sort((a, b) => b.lastAt - a.lastAt || b.id - a.id));
  const add = (input: NoticeInput, at = Date.now()) => {
    const item: Notification = { ...input, id: nextId++, at, lastAt: at, read: false, count: 1 };
    items = [...items.slice(-99), item];
    return item;
  };
  return {
    push(input: NoticeInput) { add(input); publish(); },
    // Stable conditions are announced once until they clear or materially change.
    sync(source: string, conditions: { key: string; notice: NoticeInput }[]) {
      const keys = new Set(conditions.map(c => `${source}:${c.key}`));
      for (const key of active.keys()) if (key.startsWith(`${source}:`) && !keys.has(key)) active.delete(key);
      let updated = false;
      for (const { key, notice } of conditions) {
        const name = `${source}:${key}`, signature = JSON.stringify(notice);
        if (active.get(name) === signature) continue;
        active.set(name, signature); add(notice); updated = true;
      }
      if (updated) publish();
    },
    ingestLogs(logs: Log[]) {
      let updated = false;
      for (const log of [...logs].sort((a, b) => a.at - b.at)) {
        if (seenLogs.has(log.id)) continue;
        seenLogs.add(log.id);
        // Routine discovery/cancellation and previous runs belong in logs only.
        if (log.at * 1000 < startedAt || log.operation === 'models'
          || ['success', 'cancelled', 'service_stopped'].includes(log.status)) continue;
        // Missing identities must never collapse unrelated conversations by account alone.
        const transport = log.diagnostic?.details?.origin === 'transport';
        const key = transport ? 'transport-window' : JSON.stringify([log.conversationKey || log.id, log.status]);
        const reference = {id:log.id, at:log.at * 1000, status:log.status, conversation:log.conversationKey?.split(':').at(-1)?.slice(0,12), operation:log.operation};
        const at = log.at * 1000;
        const prior = items.find(item => item.id === groups.get(key));
        if (prior && at >= prior.lastAt && at - prior.lastAt <= (transport ? WINDOW_MS : 10 * 60 * 1000)) {
          prior.count++; prior.lastAt = at; prior.requestId = log.id;
          prior.conversation = reference.conversation; prior.operation = log.operation;
          prior.requests = [...(prior.requests || []), reference];
          if(prior.requests.length > 2000) { prior.requests.shift(); prior.omittedRequests = (prior.omittedRequests || 0) + 1; }
          // An acknowledged incident stays read when the same error repeats in this window.
        } else {
          const level = ['upstream_rate_limited', 'account_unavailable'].includes(log.status) ? 'warning' : 'error';
          const item = add({ title: transport ? '连接暂不稳定，部分请求未完成' : requestStatusName(log.status), message: transport ? '以下请求发生了传输异常，未自动重放。它们按时间汇总，不表示已确认相同原因。' : log.advice || '请求未成功完成，请查看请求日志。执行状态未确认时请勿直接重放。', level }, at);
          item.requests = [reference]; item.requestId = log.id; item.conversation = log.conversationKey?.split(':').at(-1)?.slice(0, 12); item.operation = log.operation;
          groups.set(key, item.id);
        }
        updated = true;
      }
      // Bound bookkeeping as well as visible history. A polling snapshot contains at most 200 logs.
      if (seenLogs.size > 4000) for (const id of [...seenLogs].slice(0, seenLogs.size - 2000)) seenLogs.delete(id);
      const retained = new Set(items.map(item => item.id));
      for (const [key, id] of groups) if (!retained.has(id)) groups.delete(key);
      if (updated) publish();
    },
    markRead(id: number) { items = items.map(item => item.id === id ? { ...item, read: true } : item); publish(); },
    markAllRead() { items = items.map(item => ({ ...item, read: true })); publish(); },
  };
}
