import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './local-projects.css';

type Entry = { id: string; name: string; roots: string[]; order: number; present: boolean; available: boolean; canRestore: boolean };
type Status = { entries: Entry[]; accountReady: boolean; notice: string };
type Outcome = { restored: number; alreadyPresent: number; skippedUnavailable: number; skippedMultiRoot: number; remaining: number; pendingConfirmation: boolean; stopped: boolean; message: string };
type LocalChat = { id: string; name: string | null; updatedAt: number; pageCursor: string | null; archived: boolean };

export function LocalProjectsCard({ disabled }: { disabled: boolean }) {
  const [data, setData] = useState<Status | null>(null);
  const [reading, setReading] = useState(true);
  const [restoring, setRestoring] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<Outcome | null>(null);
  const [chats, setChats] = useState<LocalChat[]>([]);
  const [chatCursor, setChatCursor] = useState<string | null>(null);
  const [selectedChat, setSelectedChat] = useState('');
  const [archived, setArchived] = useState(false);
  const [chatBusy, setChatBusy] = useState(false);
  const [chatRead, setChatRead] = useState(false);
  const [chatError, setChatError] = useState<string | null>(null);
  const [chatResult, setChatResult] = useState<string | null>(null);
  const chatGeneration = useRef(0);
  const generation = useRef(0);
  const mounted = useRef(true);

  async function refresh() {
    const revision = ++generation.current;
    setReading(true);
    try {
      const next = await invoke<Status>('local_projects_status');
      if (mounted.current && revision === generation.current) { setData(next); setError(null); }
    } catch (cause) {
      if (mounted.current && revision === generation.current) setError(String(cause));
    } finally {
      if (mounted.current && revision === generation.current) setReading(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; generation.current++; chatGeneration.current++; };
  }, []);

  async function restore() {
    if (restoring || disabled) return;
    setRestoring(true); setError(null); setResult(null);
    try {
      const outcome = await invoke<Outcome>('restore_local_projects');
      if (!mounted.current) return;
      setResult(outcome);
      await refresh();
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    } finally {
      if (mounted.current) setRestoring(false);
    }
  }

  async function loadChats(more = false, includeArchived = archived) {
    const revision = ++chatGeneration.current;
    const cursor = more ? chatCursor : null;
    setChatBusy(true); setChatError(null); setChatResult(null);
    try {
      const page = await invoke<{ data: Omit<LocalChat, 'pageCursor' | 'archived'>[]; nextCursor: string | null }>('handoff_chats', { cursor, archived: includeArchived });
      if (!mounted.current || revision !== chatGeneration.current) return;
      const rows = page.data.map(chat => ({ ...chat, pageCursor: cursor, archived: includeArchived }));
      setChats(previous => more ? [...previous, ...rows.filter(row => !previous.some(p => p.id === row.id))] : rows);
      setChatCursor(page.nextCursor); setChatRead(true);
      if (!more) setSelectedChat('');
    } catch (cause) {
      if (mounted.current && revision === chatGeneration.current) setChatError(String(cause));
    } finally {
      if (mounted.current && revision === chatGeneration.current) setChatBusy(false);
    }
  }

  async function openChat() {
    const chat = chats.find(row => row.id === selectedChat);
    if (!chat || chatBusy || disabled) return;
    setChatBusy(true); setChatError(null); setChatResult(null);
    try {
      await invoke('open_local_chat', { id: chat.id, cursor: chat.pageCursor, archived: chat.archived });
      if (mounted.current) setChatResult('已请求 Codex 打开原聊天；没有新建、复制或发送聊天。能否继续发送仍取决于原聊天的账号与上下文。');
    } catch (cause) {
      if (mounted.current) setChatError(String(cause));
    } finally {
      if (mounted.current) setChatBusy(false);
    }
  }

  const missing = data?.entries.filter(entry => !entry.present && entry.canRestore).length ?? 0;
  const present = data?.entries.filter(entry => entry.present).length ?? 0;
  return <section className="local-projects-card" aria-label="本机项目">
    <div className="section-head"><h2>本机项目</h2><span className="badge neutral">目录入口</span></div>
    <p>Codex 更换个人登录后会显示不同的项目与聊天视图。这里记住本机目录，可将目录重新添加到当前登录的项目入口。</p>
    {data && <p className="local-project-count">已显示 {present} / 本机记忆 {data.entries.length} 个项目{missing > 0 ? ` · 可恢复 ${missing} 个入口` : ''}</p>}
    {!data && reading ? <p role="status">正在读取本机目录…</p> : data?.entries.length === 0 ? <p className="hint">尚未找到本机项目目录。先在 Codex 打开本地项目，再刷新目录。</p> : data && <details className="local-project-details"><summary>查看本机目录（{data.entries.length}）</summary><ul className="local-project-list">{data.entries.map(entry => <li key={entry.id}>
      <div><strong>{entry.name}</strong><span>{entry.present ? '已显示' : !entry.available ? '目录暂不可用' : !entry.canRestore ? '多目录分组，请手动打开' : '待恢复入口'}</span></div>
      {entry.roots.map(root => <code key={root}>{root}</code>)}
    </li>)}</ul></details>}
    <p className="hint" id="local-project-restore-notice">{data?.notice || '恢复目录会将 Codex 切到空白聊天输入页。请先保存草稿；此操作不会发送消息。'}</p>
    <p className="hint">这里不恢复或迁移原账号聊天。下方可查找仍保留的本机历史，并在 Codex 打开原聊天；目录入口恢复不代表旧聊天已能跨账号续接。</p>
    {data && !data.accountReady && <p className="hint">请先在 Codex 完成登录，再刷新目录。</p>}
    {disabled && <p className="hint">请等待接入切换和当前任务结束后恢复目录。</p>}
    {error && <p className="warning-text" role="alert">{error}{data ? '；保留上次目录列表。' : ''}</p>}
    {restoring && <p role="status" aria-live="polite">正在逐个恢复目录入口，请在 Codex 留意目录信任提示…</p>}
    {result && <p role="status" aria-live="polite">已确认恢复 {result.restored} 个入口。{result.message}{result.skippedUnavailable > 0 ? ` 暂不可用 ${result.skippedUnavailable} 个。` : ''}{result.skippedMultiRoot > 0 ? ` 多目录分组 ${result.skippedMultiRoot} 个，请手动打开。` : ''}</p>}
    <div className="button-row"><button disabled={reading || restoring} aria-busy={reading} onClick={() => void refresh()}>{reading ? '读取中…' : '刷新目录'}</button><button className="local-project-restore" disabled={disabled || restoring || chatBusy || reading || !data?.accountReady || !missing} aria-busy={restoring} aria-describedby="local-project-restore-notice" onClick={() => void restore()}>{restoring ? '正在恢复…' : '恢复本机项目入口'}</button></div>
    <details className="local-chat-history"><summary>查找本机历史聊天</summary>
      <p className="hint" id="local-chat-open-notice">只读取聊天标题与时间，并在 Codex 打开所选原聊天。打开会切换当前页面，请先保存草稿；不会迁移账号或发送消息。</p>
      <label className="local-chat-archive"><input type="checkbox" checked={archived} disabled={chatBusy || restoring} onChange={e => { const next = e.target.checked; setArchived(next); setChats([]); setSelectedChat(''); setChatCursor(null); setChatRead(false); chatGeneration.current++; setChatError(null); setChatResult(null); }} />已归档聊天</label>
      <div className="button-row"><button disabled={chatBusy || restoring} aria-busy={chatBusy} onClick={() => void loadChats()}>{chatBusy ? '处理中…' : '读取本机历史'}</button>{chatCursor && <button disabled={chatBusy || restoring} onClick={() => void loadChats(true)}>读取更多聊天</button>}</div>
      {chatRead && chats.length === 0 && <p className="hint">当前本机目录没有找到{archived ? '已归档' : ''}聊天；这里不查询其他电脑或云端历史。</p>}
      {chats.length > 0 && <label className="local-chat-select">本机聊天<select value={selectedChat} disabled={chatBusy || restoring} onChange={e => { setSelectedChat(e.target.value); setChatResult(null); }}><option value="">请选择聊天</option>{chats.map(chat => <option key={chat.id} value={chat.id}>{chat.name?.trim() || '未命名聊天'} · {new Date(chat.updatedAt * 1000).toLocaleDateString('zh-CN')}</option>)}</select></label>}
      {chatError && <p className="warning-text" role="alert">{chatError}</p>}
      {chatResult && <p role="status" aria-live="polite">{chatResult}</p>}
      <div className="button-row"><button disabled={disabled || restoring || chatBusy || !data?.accountReady || !selectedChat} aria-describedby="local-chat-open-notice" onClick={() => void openChat()}>在 Codex 打开聊天</button></div>
    </details>
  </section>;
}
