import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Modal } from './Modal';
import './updates.css';

export type UpdateStatus = {
  currentVersion: string; phase: string; version: string | null; notes: string | null;
  downloaded: number; total: number | null; message: string | null;
  resumeMessage?: string | null; installing: boolean; operationActive: boolean; startupPromptPending: boolean;
};
const phases: Record<string, string> = {
  idle: '等待检查更新', checking: '正在检查更新…', current: '当前已是最新版本', available: '发现新版本',
  downloading: '正在下载并校验更新…', preparing: '正在准备上一版本的修复文件…',
  finalizing: '正在准备安全安装…',
  closing: '正在恢复接入并等待请求结束…', finishing: '正在等待后台授权和保存完成…',
  installing: '正在启动安装器…', failed: '更新未完成',
};
export function useUpdates(onInstalling: (value: boolean) => void) {
  const [state, setState] = useState<UpdateStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [visible, setVisible] = useState(false);
  const [pending, setPending] = useState(false);
  const [checkingPending, setCheckingPending] = useState(false);
  const [notice, setNotice] = useState('');
  const pendingRef = useRef(false), suppressed = useRef(false), resumeShown = useRef(false), manual = useRef(false);
  const mounted = useRef(true), revision = useRef(0);
  const read = useCallback(async () => {
    const rev = ++revision.current;
    try {
      const next = await invoke<UpdateStatus>('update_status');
      if (!mounted.current || rev !== revision.current) return;
      setState(next); setError(value => value === '暂时无法读取更新状态' ? null : value);
    } catch { if (mounted.current && rev === revision.current) setError('暂时无法读取更新状态'); }
  }, []);
  useEffect(() => {
    mounted.current = true;
    let reading = false;
    const poll = async () => { if (reading) return; reading = true; try { await read(); } finally { reading = false; } };
    void poll(); const timer = setInterval(() => void poll(), 1000);
    return () => { mounted.current = false; revision.current++; clearInterval(timer); };
  }, [read]);
  useEffect(() => { onInstalling(!!state?.installing); }, [state?.installing, onInstalling]);
  useEffect(() => {
    if (state?.startupPromptPending && !suppressed.current) setVisible(true);
    if (state?.resumeMessage && !resumeShown.current) { resumeShown.current = true; setVisible(true); }
    if (manual.current && state?.phase === 'current' && !state.operationActive && !pendingRef.current) {
      manual.current = false; setVisible(false); setNotice('当前已是最新版本');
    }
  }, [state, checkingPending]);
  useEffect(() => { if (!notice) return; const t = setTimeout(() => setNotice(''), 5000); return () => clearTimeout(t); }, [notice]);
  const busy = pending || !!state?.operationActive || !!state?.installing;
  const checking = checkingPending || state?.phase === 'checking';
  const dismiss = async () => {
    if (state?.installing) return;
    try {
      await invoke('dismiss_update_prompt');
      suppressed.current = true; manual.current = false; setVisible(false); setError(null);
      await read();
    } catch (e) { if (mounted.current) { setError(String(e)); await read(); } }
  };
  const check = async () => {
    setVisible(true);
    if (pendingRef.current || busy) return;
    suppressed.current = true; manual.current = true;
    if (checking) return;
    pendingRef.current = true; setCheckingPending(true); setError(null); setNotice('');
    try { await invoke('check_update'); await read(); }
    catch (e) { if (mounted.current) { manual.current = false; setError(String(e)); } }
    finally { pendingRef.current = false; if (mounted.current) setCheckingPending(false); }
  };
  const install = async () => {
    if (pendingRef.current || busy || checking || !state?.version) return;
    pendingRef.current = true; setPending(true); setError(null); setNotice('');
    suppressed.current = true; manual.current = true; setVisible(true); revision.current++;
    try { await invoke('install_update', { version: state.version }); await read(); }
    catch (e) { if (mounted.current) { manual.current = false; setError(String(e)); } }
    finally { pendingRef.current = false; if (mounted.current) { setPending(false); await read(); } }
  };
  const openRepository = async () => {
    try { await invoke('open_project_repository'); }
    catch { setNotice('无法打开系统浏览器，请稍后重试'); }
  };
  const repair = async () => {
    try { await invoke('open_update_repair'); }
    catch (e) { setError(String(e)); }
  };
  return { state, error, visible, busy, checking, notice, dismiss, check, install, repair, openRepository };
}
type Controller = ReturnType<typeof useUpdates>;
function UpdateIcon() {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 3v12m-4-4 4 4 4-4M5 16v4h14v-4" /></svg>;
}
export function UpdateButton({ controller: c }: { controller: Controller }) {
  const available = !!c.state?.version;
  const checking = c.checking;
  const label = c.busy ? '查看更新进度' : checking ? '正在检查更新' : '检查更新';
  const failed = !!c.error || c.state?.phase === 'failed';
  const title = c.notice || c.error || c.state?.message || (available
    ? `发现 Fusion ${c.state?.version}，点击检查并查看更新说明`
    : phases[c.state?.phase || 'idle']);
  return <div className="header-update">
    <button className={`update-press ${available ? 'has-update' : failed ? 'has-error' : ''}`} aria-label={label}
      aria-busy={c.busy || checking}
      title={title || '检查最新稳定版，确认后下载安装'} onClick={() => void c.check()}>
      <span className="update-visual" aria-hidden="true"><span className="update-back" /><span className="update-front"><UpdateIcon /></span></span>
    </button>
    <span className="update-status" role="status">{c.notice}</span>
  </div>;
}
export function RepositoryButton({ controller: c }: { controller: Controller }) {
  return <button className="repository-link" aria-label="LEARN MORE · 打开 Fusion GitHub 仓库" title="打开 Fusion GitHub 仓库" onClick={() => void c.openRepository()}>
    <span className="repository-circle" aria-hidden="true"><span className="repository-arrow" /></span><span className="repository-text">LEARN MORE</span>
  </button>;
}
function UpdateNotes({ notes }: { notes: string }) {
  // Only headings, lists and paragraphs. Remote text is never interpreted as HTML.
  return <div className="update-notes">{notes.split(/\r?\n/).filter(line => line.trim()).map((line, i) => {
    if (/^#{1,3}\s/.test(line)) return <h3 key={i}>{line.replace(/^#{1,3}\s+/, '')}</h3>;
    if (/^[-*]\s/.test(line)) return <p className="update-note-item" key={i}>{line.replace(/^[-*]\s+/, '')}</p>;
    return <p key={i}>{line}</p>;
  })}</div>;
}
export function UpdateDialog({ controller: c }: { controller: Controller }) {
  if (!c.visible) return null;
  const s = c.state, failed = !!c.error || s?.phase === 'failed' || !!s?.resumeMessage;
  const progress = s?.total ? Math.min(100, Math.round(s.downloaded / s.total * 100)) : undefined;
  return <Modal label="Fusion 软件更新" busy={!!s?.installing} onClose={() => void c.dismiss()}>
    <article className="update-card">
      <div className="update-card-icon"><UpdateIcon /></div>
      <h2>{c.busy ? '正在更新 Fusion' : c.checking ? '正在检查更新' : failed ? '更新需要处理' : s?.version ? 'Fusion 有新版本了' : 'Fusion 软件更新'}</h2>
      <p className="update-versions">当前 v{s?.currentVersion || '…'}{s?.version && <> <span aria-hidden="true">→</span> <strong>v{s.version}</strong></>}</p>
      <div className="update-card-body">
        {s?.notes && <UpdateNotes notes={s.notes} />}
        {(c.busy || c.checking) && <div className="update-progress" role="status" aria-live="polite"><p>{phases[s?.phase || 'checking'] || '正在准备更新…'}</p>
          {(s?.phase === 'downloading' || s?.phase === 'preparing') && <><progress aria-label="更新包下载进度" value={progress} max={100} /><small>{(s.downloaded / 1048576).toFixed(1)} MiB{s.total ? ` / ${(s.total / 1048576).toFixed(1)} MiB` : ''}</small></>}
        </div>}
        {(c.error || s?.message) && <p className="update-error" role="alert">{c.error || s?.message}</p>}
        {s?.resumeMessage && <p className="update-error" role="alert">{s.resumeMessage}</p>}
      </div>
      <p className="update-caution">账号池已关闭时，更新不会关闭或重启 Codex。账号池开启时，请先完成任务并保存输入；更新前会等待在途请求完成、恢复原接入并关闭 Codex。更新后账号池保持关闭，点击“启动账号池”才打开 Codex。</p>
      <div className="update-card-actions">
        <button className="update-later" disabled={!!s?.installing} onClick={() => void c.dismiss()}>稍后更新</button>
        <button className="update-now" disabled={c.busy || c.checking || !s?.version} onClick={() => void c.install()}>{c.busy ? '更新进行中…' : failed ? '重试更新' : '立即更新'}</button>
      </div>
      {failed && <button className="update-repair" disabled={c.busy} onClick={() => void c.repair()}>打开修复目录</button>}
    </article>
  </Modal>;
}
