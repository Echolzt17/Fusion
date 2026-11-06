import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './handoff.css';

type Chat = { id: string; name: string | null; provider: string; updatedAt: number };
type Record = { id: string; sourceId: string; target: string; accountId: string | null; targetId: string | null; phase: string; message: string };
type Preview = { id: string; history: { message: string; turns: number; totalTurns: number; warnings: string[]; attachments: string[] } };
export function ConversationHandoff({ mode, accounts, disabled }: {
  mode: string; accounts: { id: string; email: string; state: string }[]; disabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const [chats, setChats] = useState<Chat[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [archived, setArchived] = useState(false);
  const [source, setSource] = useState('');
  const [target, setTarget] = useState(mode === 'on' ? 'pool' : 'official');
  const [account, setAccount] = useState('');
  const [range, setRange] = useState(0);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [records, setRecords] = useState<Record[]>([]);
  const [working, setWorking] = useState('');
  const [error, setError] = useState('');
  const workingRef = useRef(false);
  const revision = useRef(0);
  const stable = mode === 'on' || mode === 'off';
  const matched = target === 'pool' ? mode === 'on' : mode === 'off';
  async function status() { setRecords(await invoke<Record[]>('handoff_status')); }
  async function run(label: string, work: () => Promise<void>) {
    if (workingRef.current) return;
    workingRef.current = true; setWorking(label); setError('');
    try { await work(); }
    catch (e) { setError(String(e)); }
    finally { try { await status(); } catch { /* Keep the actionable operation error. */ } workingRef.current = false; setWorking(''); }
  }
  const clear = () => { revision.current++; setPreview(null); };
  async function load(more = false, includeArchived = archived) {
    const result = await invoke<{ data: Chat[]; nextCursor: string | null }>('handoff_chats', { cursor: more ? cursor : null, archived: includeArchived });
    setChats(old => more ? [...old, ...result.data.filter(chat => !old.some(c => c.id === chat.id))] : result.data);
    setCursor(result.nextCursor);
  }
  useEffect(() => { clear(); if (mode === 'on' || mode === 'off') setTarget(mode === 'on' ? 'pool' : 'official'); }, [mode]);
  const current = preview ? records.find(r => r.id === preview.id) : undefined;
  return <section className="handoff-section" aria-label="跨接入聊天续接">
    <div className="section-head"><div><h2>聊天续接</h2><p className="subtle">将可读历史带到新的聊天，继续使用选定账号。</p></div>
      <button aria-expanded={expanded} onClick={() => { const next = !expanded; setExpanded(next); if (next) void run('读取聊天', async () => { await load(); }); }}> {expanded ? '收起续接' : '选择聊天续接'}</button></div>
    {expanded && <>
      <p className="hint">原聊天保留。新聊天承接一条交接消息；密文和账号专属远端引用不迁移。官方续接完成后可完全退出 Fusion。</p>
      <div className="handoff-fields"><label>源聊天<select aria-label="源聊天" value={source} disabled={!!working} onChange={e => { setSource(e.target.value); clear(); }}>
        <option value="">请选择聊天</option>{chats.map(chat => <option key={chat.id} value={chat.id}>{chat.name || '未命名聊天'} · {chat.provider}</option>)}
      </select></label>
      <label>目标接入<select aria-label="目标接入" value={target} disabled={!!working} onChange={e => { setTarget(e.target.value); clear(); }}><option value="pool">账号池</option><option value="official">官方接入</option></select></label>
      {target === 'pool' && <label>目标账号<select aria-label="目标账号" value={account} disabled={!!working} onChange={e => { setAccount(e.target.value); clear(); }}><option value="">请选择账号</option>{accounts.filter(a => a.state === 'ready').map(a => <option key={a.id} value={a.id}>{a.email}</option>)}</select></label>}
      <label>交接范围<select aria-label="交接范围" value={range} disabled={!!working} onChange={e => { setRange(Number(e.target.value)); clear(); }}><option value={0}>全部可读轮次</option><option value={50}>最近 50 轮</option><option value={20}>最近 20 轮</option><option value={10}>最近 10 轮</option></select></label></div>
      <div className="button-row"><label className="handoff-archive"><input type="checkbox" checked={archived} disabled={!!working} onChange={e => { const next = e.target.checked; setArchived(next); setSource(''); clear(); void run('读取聊天', () => load(false, next)); }} />已归档聊天</label>
        <button disabled={!!working} onClick={() => void run('刷新聊天', () => load())}>刷新列表</button>{cursor && <button disabled={!!working} onClick={() => void run('读取更多聊天', () => load(true))}>更多聊天</button>}
        <button disabled={disabled || !!working || !stable || !matched || !source || target === 'pool' && !account} onClick={() => void run('准备交接', async () => {
          const version = revision.current;
          const result = await invoke<Preview>('handoff_prepare', { sourceId: source, target, accountId: target === 'pool' ? account : null, recentTurns: range });
          if (version === revision.current) setPreview(result);
        })}>准备交接</button></div>
      {!matched && <p role="status">请先{target === 'pool' ? '启动账号池' : '关闭账号池并恢复官方接入'}，再准备交接。</p>}
      {preview && <div className="handoff-preview"><strong>将承接 {preview.history.turns} 轮{preview.history.totalTurns ? ` / 共 ${preview.history.totalTurns} 轮` : '（更早历史未读取）'} · 本地附件 {preview.history.attachments.length} 项</strong>
        {preview.history.warnings.length > 0 && <ul>{preview.history.warnings.map(w => <li key={w}>{w}</li>)}</ul>}
        <details><summary>查看完整交接消息</summary><pre>{preview.history.message}</pre></details>
        <div className="button-row">{(!current || current.phase === 'prepared') && <button disabled={disabled || !!working || !matched} onClick={() => void run('创建续接聊天', async () => { await invoke('handoff_create', { id: preview.id }); })}>创建续接聊天</button>}
          {current?.phase === 'created' && <button disabled={disabled || !!working || !matched} onClick={() => void run('发送交接，等待目标账号确认', async () => { await invoke('handoff_send', { id: preview.id }); })}>发送交接并等待确认</button>}
          {(!current || ['prepared', 'created'].includes(current.phase)) && <button disabled={!!working} onClick={() => void run('取消交接', async () => { await invoke('handoff_cancel', { id: preview.id }); setPreview(null); })}>取消交接</button>}</div>
        <p className="subtle">创建聊天不发送模型请求；发送交接会使用目标账号。发送失败或结果未确认时不会自动重发。</p>
      </div>}
      {working && <p role="status">{working}…</p>}{error && <p role="alert">{error}</p>}
      {records.length > 0 && <details className="handoff-records" open><summary>最近续接记录</summary>{records.slice(0, 5).map(record => <div key={record.id}><span>{record.target === 'pool' ? '账号池' : '官方'} · {record.message}</span>{record.targetId && <button disabled={!!working || disabled || (record.target === 'pool' ? mode !== 'on' : mode !== 'off')} onClick={() => void run('打开续接聊天', async () => { await invoke('handoff_open', { id: record.id }); })}>打开目标聊天</button>}</div>)}</details>}
    </>}
  </section>;
}
