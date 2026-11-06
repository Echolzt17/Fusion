import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Modal } from './Modal';
import type { Account } from './PoolTelemetry';

export type ActiveRequest = {
  id: string; operation: string; account: string | null; conversationKey: string | null;
  phase: string; elapsedMs: number; idleMs: number;
};
const phases: Record<string, string> = {
  uploading: '接收请求', ingress_queue: '等待空闲名额', queued: '等待空闲名额', authorization: '准备授权', proxy: '准备连接',
  cooldown: '等待账号短期冷却结束，尚未发送（最多 60 秒）',
  headers: '等待上游响应', reading: '接收数据', streaming: '接收事件流',
  reconnecting: '连接暂不稳定，正在重新连接', finishing: '完成收尾', cancelling: '正在取消',
};
const operations: Record<string, string> = {
  responses: '对话', compact: '上下文压缩', image_generate: '图片生成', image_edit: '图片编辑', models: '模型检查',
};
export function ActiveRequests({ requests, accounts, onChanged }: {
  requests: ActiveRequest[]; accounts: Account[]; onChanged: () => void;
}) {
  const [selected, setSelected] = useState<ActiveRequest | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  if (!requests.length && !selected) return null;
  const queued = requests.filter(r => r.phase === 'queued' || r.phase === 'ingress_queue' || r.phase === 'cooldown').length;
  async function cancel() {
    if (!selected || busy) return;
    setBusy(true); setError('');
    try { await invoke('cancel_request', { id: selected.id }); setSelected(null); onChanged(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  return <section className="active-requests" aria-label="当前请求">
    <div className="section-head"><h2>当前请求</h2><span>处理中 {requests.length - queued} · 排队 {queued}</span></div>
    <p className="subtle">繁忙或账号短期冷却时先等待，已有会话保持原账号；不再需要的任务可定向取消。已发送的请求不会自动重发。</p>
    <div className="active-request-list">{requests.map(request => {
      const account = accounts.findIndex(a => a.id === request.account);
      const conversation = request.conversationKey?.split(':').at(-1)?.slice(0, 12);
      return <article key={request.id} className="active-request">
        <div><strong>{operations[request.operation] || '请求'} · {account >= 0 ? '账号 ' + (account + 1) : '待分配账号'}</strong>
          <p>{phases[request.phase] || '处理中'} · 已用 {Math.floor(request.elapsedMs / 1000)} 秒 · {conversation ? '对话 ' + conversation : '未识别对话'}</p>
          <small>状态更新 {Math.floor(request.idleMs / 1000)} 秒前 · 请求 {request.id}</small></div>
        <button disabled={request.phase === 'cancelling' || request.phase === 'finishing' || busy}
          onClick={() => { setError(''); setSelected(request); }}>取消请求</button>
      </article>;
    })}</div>
    {selected && <Modal label="取消请求" busy={busy} onClose={() => setSelected(null)}>
      <div className="modal"><h2>取消此请求并释放连接</h2>
        <p>只处理选中的本机请求。已发送的上游任务是否停止无法保证，取消后不会自动重发。</p>
        <p className="subtle">请求 {selected.id}</p>
        {error && <p role="alert">{error}</p>}
        <div className="button-row"><button disabled={busy} onClick={() => setSelected(null)}>继续执行</button>
          <button disabled={busy} onClick={() => void cancel()}>{busy ? '正在释放…' : '确认取消并释放'}</button></div>
      </div>
    </Modal>}
  </section>;
}
