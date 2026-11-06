import { useEffect, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

declare const __APP_VERSION__: string;

export function WindowChrome() {
  const [maximized, setMaximized] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!isTauri()) return;
    const win = getCurrentWindow();
    let disposed = false;
    const update = async () => {
      try { const value = await win.isMaximized(); if (!disposed) setMaximized(value); }
      catch { if (!disposed) setError('无法读取窗口状态'); }
    };
    void update();
    const listener = win.onResized(() => void update());
    return () => { disposed = true; void listener.then(unlisten => unlisten()); };
  }, []);
  async function control(action: 'minimize' | 'toggleMaximize' | 'close') {
    if (!isTauri()) return;
    try { await getCurrentWindow()[action](); setError(''); }
    catch { setError('窗口操作失败，请重试'); }
  }
  return <div className="window-chrome">
    <div className="window-drag" data-tauri-drag-region><img className="window-mark" src="/fusion.svg" alt="" aria-hidden="true" /><span>Fusion {__APP_VERSION__}</span></div>
    {error && <span role="alert" className="window-error">{error}</span>}
    <div className="window-buttons">
      <button aria-label="最小化" title="最小化" onClick={() => void control('minimize')}><svg viewBox="0 0 16 16"><path d="M3 8h10" /></svg></button>
      <button aria-label={maximized ? '还原窗口' : '最大化'} title={maximized ? '还原窗口' : '最大化'} onClick={() => void control('toggleMaximize')}><svg viewBox="0 0 16 16">{maximized ? <path d="M5 5V3h8v8h-2M3 5h8v8H3z" /> : <path d="M3.5 3.5h9v9h-9z" />}</svg></button>
      <button className="window-close" aria-label="关闭到托盘" title="关闭到托盘" onClick={() => void control('close')}><svg viewBox="0 0 16 16"><path d="m3.5 3.5 9 9m0-9-9 9" /></svg></button>
    </div>
  </div>;
}
