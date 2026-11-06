import { useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Modal } from './Modal';
import type { NetworkSettings, NetworkStatus, NodeView, SubscriptionView } from './network-types';
import './network-cards.css';

const MAX_FILE_BYTES = 8 * 1024 * 1024;
const regionNames: Record<string, string> = {
  US: '美国', JP: '日本', SG: '新加坡', HK: '香港', TW: '台湾', KR: '韩国', GB: '英国',
  DE: '德国', FR: '法国', CA: '加拿大', AU: '澳大利亚', NL: '荷兰', CH: '瑞士',
  SE: '瑞典', NO: '挪威', FI: '芬兰', DK: '丹麦', IE: '爱尔兰', IT: '意大利',
  ES: '西班牙', PT: '葡萄牙', AT: '奥地利', BE: '比利时', PL: '波兰', CZ: '捷克',
  IN: '印度', MY: '马来西亚', TH: '泰国', VN: '越南', PH: '菲律宾', ID: '印度尼西亚',
  NZ: '新西兰', BR: '巴西', MX: '墨西哥', AR: '阿根廷', CL: '智利', TR: '土耳其',
  IL: '以色列', AE: '阿联酋', ZA: '南非', RU: '俄罗斯', CN: '中国大陆', UNKNOWN: '未分类',
};
const normalRegion = (region: string) => !region || ['unknown', '未知'].includes(region.toLowerCase()) ? 'UNKNOWN' : region.toUpperCase();
const regionLabel = (region: string) => regionNames[normalRegion(region)] ?? normalRegion(region);
const healthNames: Record<NodeView['health'], string> = { unknown: '待检测', healthy: '基础健康', degraded: '连接不稳定', unhealthy: '暂不可用' };
const phases: Record<string, string> = {
  idle: '等待使用', off: '未启用', disabled: '未启用', stopped: '已停止', ready: '已就绪', running: '运行中',
  starting: '正在启动内核', stopping: '正在停止内核', checking: '正在检测节点', importing: '正在验证订阅',
  updating: '正在更新订阅', refreshing: '正在更新订阅', downloading: '正在下载订阅', draining: '等待在途请求完成',
  applying: '正在应用配置', failed: '需要处理', error: '需要处理', unavailable: '暂无可用节点',
};
const date = (value: number | null) => value && Number.isFinite(value) ? new Date(value < 1e12 ? value * 1000 : value).toLocaleString('zh-CN', { hour12: false }) : '尚未记录';
const copySettings = (value: NetworkSettings): NetworkSettings => ({ ...value, allowedRegions: [...value.allowedRegions], nodeOrder: [...value.nodeOrder], excludedNodeIds: [...value.excludedNodeIds], regionOverrides: { ...value.regionOverrides } });
const settingsKey = (value: NetworkSettings) => JSON.stringify(value);
const safeError = (error: unknown, secrets: string[] = []) => {
  let value = String(error);
  for (const secret of secrets) if (secret) value = value.split(secret).join('[已隐藏]');
  // A backend failure must never make an entered subscription URL visible.
  return value.replace(/https?:\/\/[^\s<>"']+/gi, '[订阅地址已隐藏]').slice(0, 900);
};

type Props = { poolRunning: boolean; onError: (message: string) => void; onNotice?: (message: string) => void };

export function NetworkCards({ poolRunning, onError, onNotice }: Props) {
  const [status, setStatus] = useState<NetworkStatus | null>(null);
  const [draft, setDraft] = useState<NetworkSettings | null>(null);
  const [readError, setReadError] = useState('');
  const [operation, setOperation] = useState('');
  const [link, setLink] = useState('');
  const [name, setName] = useState('');
  const [importOpen, setImportOpen] = useState(false);
  const [removing, setRemoving] = useState<SubscriptionView | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [editNode, setEditNode] = useState<NodeView | null>(null);
  const [editRegion, setEditRegion] = useState('UNKNOWN');
  const [editError, setEditError] = useState('');
  const [filter, setFilter] = useState('');
  const [localError, setLocalError] = useState('');
  const fileInput = useRef<HTMLInputElement>(null);
  const linkInput = useRef<HTMLInputElement>(null);
  const mounted = useRef(false);
  const revision = useRef(0);
  const reading = useRef(false);
  const operating = useRef(false);
  const savedSettings = useRef('');
  const selectedSubscription = useRef<string | null>(null);
  const dirtyRef = useRef(false);

  const accept = useCallback((next: NetworkStatus, replaceDraft = false) => {
    if (!next?.settings || !Array.isArray(next.nodes)) throw new Error('网络状态暂不可用，请重试');
    if (!mounted.current) return;
    setStatus(next);
    setReadError('');
    savedSettings.current = settingsKey(next.settings);
    const changedSubscription = selectedSubscription.current !== next.selectedSubscriptionId;
    selectedSubscription.current = next.selectedSubscriptionId;
    if (changedSubscription) { setFilter(''); setEditNode(null); }
    if (replaceDraft || changedSubscription || !dirtyRef.current) {
      setDraft(copySettings(next.settings));
      dirtyRef.current = false;
    }
  }, []);

  const refresh = useCallback(async () => {
    if (reading.current || operating.current) return;
    reading.current = true;
    const version = revision.current;
    try {
      const next = await invoke<NetworkStatus>('network_status');
      if (version === revision.current) accept(next);
    } catch (error) {
      if (mounted.current && version === revision.current) setReadError(safeError(error));
    } finally { reading.current = false; }
  }, [accept]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const timer = window.setInterval(() => { if (document.visibilityState !== 'hidden') void refresh(); }, 3000);
    return () => { mounted.current = false; revision.current++; window.clearInterval(timer); };
  }, [refresh]);

  useEffect(() => { if (importOpen) linkInput.current?.focus(); }, [importOpen]);

  const run = async (command: string, args?: Record<string, unknown>, notice?: string, secrets: string[] = []) => {
    if (operating.current) return false;
    operating.current = true;
    revision.current++;
    setOperation(command);
    setLocalError('');
    try {
      const next = await invoke<NetworkStatus>(command, args);
      accept(next, true);
      if (mounted.current && notice) onNotice?.(notice);
      return true;
    } catch (error) {
      const message = safeError(error, secrets);
      if (mounted.current) { setLocalError(message); onError(message); }
      return false;
    } finally {
      operating.current = false;
      if (mounted.current) setOperation('');
    }
  };

  const changeDraft = (next: NetworkSettings) => {
    dirtyRef.current = settingsKey(next) !== savedSettings.current;
    setDraft(next);
  };
  const dirty = !!draft && settingsKey(draft) !== savedSettings.current;
  const busy = !!operation;
  const blocked = busy || !!readError || !status || !draft;
  const current = status?.nodes.find(node => node.id === status.currentNodeId);
  const effectiveRegion = useCallback((node: NodeView) => normalRegion(draft?.regionOverrides[node.id] || node.region), [draft]);
  const orderedNodes = useMemo(() => {
    if (!status || !draft) return [];
    const order = new Map(draft.nodeOrder.map((id, index) => [id, index]));
    return [...status.nodes].sort((a, b) => (order.get(a.id) ?? Number.MAX_SAFE_INTEGER) - (order.get(b.id) ?? Number.MAX_SAFE_INTEGER));
  }, [status, draft]);
  const regions = useMemo(() => [...new Set([...(draft?.allowedRegions ?? []), ...(status?.nodes.map(effectiveRegion) ?? [])])].sort((a, b) => {
    const order = draft?.allowedRegions ?? [];
    return (order.indexOf(a) < 0 ? 10000 : order.indexOf(a)) - (order.indexOf(b) < 0 ? 10000 : order.indexOf(b)) || regionLabel(a).localeCompare(regionLabel(b), 'zh-CN');
  }), [draft, status, effectiveRegion]);
  const eligible = orderedNodes.filter(node => draft && !draft.excludedNodeIds.includes(node.id) && draft.allowedRegions.includes(effectiveRegion(node)));
  const searching = filter.trim().toLocaleLowerCase();
  const visibleNodes = searching ? orderedNodes.filter(node => `${node.name} ${regionLabel(effectiveRegion(node))} ${effectiveRegion(node)}`.toLocaleLowerCase().includes(searching)) : orderedNodes;
  const moveRegion = (region: string, step: number) => {
    if (!draft) return;
    const next = [...draft.allowedRegions];
    const index = next.indexOf(region);
    if (index < 0 || index + step < 0 || index + step >= next.length) return;
    [next[index], next[index + step]] = [next[index + step], next[index]];
    changeDraft({ ...draft, allowedRegions: next });
  };
  const moveNode = (node: NodeView, step: number) => {
    if (!draft) return;
    const localNodes = orderedNodes.filter(value => effectiveRegion(value) === effectiveRegion(node));
    const index = localNodes.findIndex(value => value.id === node.id);
    const other = localNodes[index + step];
    if (!other) return;
    const ids = orderedNodes.map(value => value.id);
    const first = ids.indexOf(node.id), second = ids.indexOf(other.id);
    [ids[first], ids[second]] = [ids[second], ids[first]];
    changeDraft({ ...draft, nodeOrder: ids });
  };
  const clearImport = () => { setLink(''); setName(''); if (fileInput.current) fileInput.current.value = ''; };
  const importLink = async () => {
    let url: URL;
    try { url = new URL(link.trim()); } catch { setLocalError('请输入有效的 HTTPS 订阅链接'); return; }
    if (url.protocol !== 'https:') { setLocalError('订阅链接需要使用 HTTPS'); return; }
    const value = link.trim();
    if (await run('network_import', { input: { url: value, yaml: null, name: name.trim() || null } }, '订阅已添加，可在列表中选用；当前连接保持', [value])) {
      clearImport(); setImportOpen(false);
    }
  };
  const importFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;
    if (file.size > MAX_FILE_BYTES) { setLocalError('订阅文件不能超过 8 MiB'); event.target.value = ''; return; }
    try {
      let yaml = await file.text();
      const success = await run('network_import', { input: { yaml, url: null, name: name.trim() || '本地订阅' } }, '本地订阅已添加，可在列表中选用；当前连接保持', [yaml]);
      yaml = '';
      if (success) { clearImport(); setImportOpen(false); }
    } catch { setLocalError('无法读取所选 YAML 文件，请重新选择'); }
    finally { if (fileInput.current) fileInput.current.value = ''; }
  };

  return <div className="network-card-group" role="group" aria-label="代理与节点">
    <div className="network-group-heading"><div><h2>代理与节点</h2><p>Codex 订阅池专用网络</p></div><span className={'badge ' + (status?.settings.enabled ? '' : 'neutral')}>{status ? status.settings.enabled ? '内置代理' : '系统代理' : '状态未知'}</span></div>
    {readError && <div className="network-callout network-danger" role="alert"><span>网络状态读取失败：{readError}</span><button type="button" onClick={() => void refresh()}>重试读取网络状态</button></div>}
    {!status && !readError && <p className="network-loading" role="status">正在读取网络状态…</p>}
    {status && draft && <>
      <div className="network-grid">
        <section className="network-section network-status-section" aria-label="专用网络状态">
          <div className="network-card-heading"><h3>当前连接</h3><span className={'network-kernel-dot ' + (status.kernelRunning ? 'is-running' : '')} aria-label={status.kernelRunning ? '内核运行中' : '内核未运行'} /></div>
          <strong className="network-current-node">{status.settings.enabled ? (current?.name || '等待可用节点') : '使用现有系统代理'}</strong>
          <p className="network-selected-copy">{status.subscriptionName ? `已选订阅：${status.subscriptionName}` : status.subscriptions?.length ? '从下面的订阅列表中选用一个' : '添加订阅后，即可选择代理节点'}</p>
          <div className="network-status-metrics"><span>账号池<b>{poolRunning ? '运行中' : '已关闭'}</b></span><span>专用内核<b>{status.kernelRunning ? '运行中' : '未运行'}</b></span><span>运行状态<b>{phases[status.phase] || '正在处理'}</b></span></div>
          {current && status.settings.enabled && <p className="network-muted">{regionLabel(current.region)} · {healthNames[current.health]}{current.delayMs !== null ? ` · ${current.delayMs} ms` : ''}</p>}
          {status.settings.enabled && !poolRunning && <p className="network-muted">账号池已关闭，专用网络仍供授权和额度查询使用。</p>}
          {status.lastSwitchReason && <p className="network-switch-reason">{status.lastSwitchReason}</p>}
          <div className="network-actions">
            <button type="button" className={status.settings.enabled ? '' : 'network-primary'} disabled={blocked || dirty || (!status.settings.enabled && (!status.nodes.length || !draft.allowedRegions.length))} onClick={() => void run('network_set_enabled', { enabled: !status.settings.enabled }, status.settings.enabled ? '已恢复使用系统代理' : '内置代理已启用')}>{status.settings.enabled ? '改用系统代理' : '使用内置代理'}</button>
            <button type="button" disabled={blocked || !status.settings.enabled} onClick={() => void run('network_retry', undefined, '已重新检查专用网络')}>重试网络</button>
          </div>
        </section>

        <section className="network-section network-subscription-section" aria-label="代理订阅">
          <div className="network-card-heading"><div><h3>我的订阅 <span className="network-count">{status.subscriptions?.length ?? 0}</span></h3><p className="network-muted">可保存多个，每次选用一个；添加不会替换已有订阅。</p></div><button type="button" className="network-primary" disabled={blocked || dirty} onClick={() => { setLocalError(''); setImportOpen(true); }}>添加订阅</button></div>
          {!status.subscriptions?.length ? <div className="network-empty">尚未导入订阅</div> : <div className="network-subscription-list">{status.subscriptions.map((subscription, index) => {
            const selected = subscription.id === status.selectedSubscriptionId;
            return <article className={'network-subscription-row' + (selected ? ' is-selected' : '')} key={subscription.id} aria-label={`订阅 ${subscription.name}`}>
              <span className="network-receipt-number" aria-hidden="true">{String(index + 1).padStart(2, '0')}</span>
              <div className="network-subscription-copy"><strong className="network-subscription-name">{subscription.name}</strong><p className="network-muted">{subscription.nodeCount} 个节点 · {subscription.kind === 'url' ? selected ? '链接订阅 · 每 6 小时更新' : '链接订阅 · 可手动更新' : '本地 YAML · 手动重新导入'}</p><p className="network-timestamp">更新：{date(subscription.updatedAt)}</p></div>
              <div className="network-subscription-actions"><button type="button" aria-pressed={selected} disabled={blocked || dirty || selected} onClick={() => void run('network_select_subscription', { id: subscription.id }, `已选用 ${subscription.name}`)}>{selected ? '已选用' : '选用'}</button>
                <details className="network-subscription-menu"><summary>管理<span className="network-sr-only"> {subscription.name}</span></summary><div className="network-subscription-menu-actions">
                  <button type="button" disabled={blocked || dirty || subscription.kind !== 'url'} onClick={() => void run('network_refresh', { id: subscription.id, systemProxy: false }, '订阅更新已处理')}>更新订阅</button>
                  {subscription.kind === 'url' && <button type="button" disabled={blocked || dirty} onClick={() => void run('network_refresh', { id: subscription.id, systemProxy: true }, '已通过系统代理更新订阅')}>通过系统代理更新</button>}
                  <button type="button" className="network-danger-text" disabled={blocked || dirty} onClick={() => { setLocalError(''); setRemoving(subscription); }}>移除订阅</button>
                </div></details>
              </div>
            </article>;
          })}</div>}
          {!!status.skippedNodes && <p className="network-muted">已跳过 {status.skippedNodes} 个不支持、依赖外部配置或重复的节点。</p>}
          {!!status.importWarnings?.length && <p className="network-callout" role="status">{status.importWarnings.join('；')}</p>}
        </section>

        <details className="network-advanced-settings" open={advancedOpen} onToggle={event => setAdvancedOpen(event.currentTarget.open)}>
          <summary>高级设置<span>{dirty ? '有未保存的修改' : '地区、备用顺序与节点管理'}</span></summary>
        <section className="network-section network-policy-section" aria-label="地区与切换策略">
          <div className="network-card-heading"><h3>地区与切换策略</h3><span className="network-muted">健康时保持当前节点</span></div>
          <div className="network-policy-layout"><div>
            <div className="network-mode-picker" role="group" aria-label="节点选择模式">
              <button type="button" aria-pressed={draft.mode === 'auto'} disabled={blocked} onClick={() => changeDraft({ ...draft, mode: 'auto', fixedNodeId: null })}>自动切换</button>
              <button type="button" aria-pressed={draft.mode === 'fixed'} disabled={blocked} onClick={() => changeDraft({ ...draft, mode: 'fixed', fixedNodeId: draft.fixedNodeId || status.currentNodeId })}>手动固定</button>
            </div>
            {draft.mode === 'fixed' ? <label className="network-fixed-label">固定节点<select value={draft.fixedNodeId || ''} disabled={blocked} onChange={event => changeDraft({ ...draft, fixedNodeId: event.target.value || null })}><option value="">请选择允许地区内的节点</option>{eligible.map(node => <option key={node.id} value={node.id}>{node.name} · {regionLabel(effectiveRegion(node))}</option>)}</select></label> : <p className="network-muted">连续检测失败后先在同地区换点，再按右侧顺序使用备用地区。恢复的旧节点不抢回。</p>}
            {draft.mode === 'fixed' && <p className="network-muted">固定节点故障时提示，等待恢复或由你重新选择。</p>}
          </div><div className="network-region-policy">
            <p className="network-field-title">允许地区与备用顺序</p>
            {!regions.filter(region => normalRegion(region) !== 'UNKNOWN').length && <p className="network-muted">导入后选择允许地区；未分类节点可在列表中纠正。</p>}
            {regions.filter(region => normalRegion(region) !== 'UNKNOWN').map(region => {
              const index = draft.allowedRegions.indexOf(region);
              return <div className="network-region-option" key={region}>
                <label><input type="checkbox" checked={index >= 0} disabled={blocked} onChange={event => changeDraft({ ...draft, allowedRegions: event.target.checked ? [...draft.allowedRegions, region] : draft.allowedRegions.filter(value => value !== region) })} /><span>{regionLabel(region)} <small>{region}</small></span></label>
                {index >= 0 && <><span className="network-region-priority">{index === 0 ? '主地区' : `备用 ${index}`}</span><div className="network-order-actions"><button type="button" aria-label={`上移${regionLabel(region)}地区`} disabled={blocked || index === 0} onClick={() => moveRegion(region, -1)}>↑</button><button type="button" aria-label={`下移${regionLabel(region)}地区`} disabled={blocked || index === draft.allowedRegions.length - 1} onClick={() => moveRegion(region, 1)}>↓</button></div></>}
              </div>;
            })}
          </div></div>
          <div className="network-actions network-save-row"><button type="button" className="network-primary" disabled={blocked || !dirty || (draft.mode === 'fixed' && !eligible.some(node => node.id === draft.fixedNodeId))} onClick={() => void run('network_save_settings', { settings: { ...draft, enabled: status.settings.enabled } }, '节点策略已保存')}>保存节点策略</button>{dirty && <><button type="button" disabled={blocked} onClick={() => { dirtyRef.current = false; setDraft(copySettings(status.settings)); }}>放弃修改</button><span className="network-unsaved" role="status">有未保存的策略修改</span></>}</div>
        </section></details>

        <section className="network-section network-nodes-section" aria-label="代理节点列表">
          <div className="network-card-heading"><div><h3>地区与节点</h3><p className="network-muted">仅显示当前订阅。{status.settings.mode === 'fixed' ? '已固定节点；需要自动切换时可在高级设置中调整。' : '默认自动选点，也可展开地区后手动选择。'}</p></div>{advancedOpen && <button type="button" disabled={blocked || dirty || !status.settings.enabled || !status.nodes.length} onClick={() => void run('network_check', { ids: null })}>{operation === 'network_check' ? '正在检测…' : '检测全部节点'}</button>}</div>
          {!!status.nodes.length && <label className="network-search-label">查找节点<input type="search" value={filter} onChange={event => setFilter(event.target.value)} placeholder="节点名称或地区" /></label>}
          <div className="network-region-list">
            {!visibleNodes.length && <p className="network-empty">{status.nodes.length ? '没有匹配的节点' : '导入订阅后在这里查看和选择节点'}</p>}
            {regions.map(region => {
              const nodes = visibleNodes.filter(node => effectiveRegion(node) === region);
              if (!nodes.length) return null;
              const localNodes = orderedNodes.filter(node => effectiveRegion(node) === region);
              const healthy = nodes.filter(node => node.health === 'healthy').length;
              return <details className="network-region-group" key={region}>
                <summary><span>{regionLabel(region)}</span><span className="network-region-count">{nodes.length} 个节点 · {healthy} 个基础健康</span>{draft.allowedRegions.includes(region) && <span className="network-allowed-tag">允许使用</span>}</summary>
                <div className="network-node-list">{nodes.map(node => {
                  const excluded = draft.excludedNodeIds.includes(node.id);
                  const index = localNodes.findIndex(value => value.id === node.id);
                  const selectable = !excluded && draft.allowedRegions.includes(region) && region !== 'UNKNOWN';
                  const selected = status.currentNodeId === node.id;
                  return <article className={'network-node' + (selected ? ' is-current' : '') + (excluded ? ' is-excluded' : '')} key={node.id} aria-label={`节点 ${node.name}`}>
                    <div className="network-node-main"><div className="network-node-heading"><strong>{node.name}</strong>{selected && <span className="network-current-tag">当前节点</span>}{excluded && <span className="network-muted">已排除</span>}</div>
                      <div className="network-node-health"><span className={`network-health network-health-${node.health}`}>{healthNames[node.health]}</span>{node.delayMs !== null && <span>{node.delayMs} ms</span>}<span>检测：{date(node.lastCheckedAt)}</span>{node.cooldownUntil !== null && node.cooldownUntil * 1000 > Date.now() && <span>冷却至 {date(node.cooldownUntil)}</span>}</div>
                      <div className="network-node-business">近期业务：{node.businessSuccesses + node.businessFailures > 0 ? `成功 ${node.businessSuccesses} · 失败 ${node.businessFailures}` : '尚无记录'}<span>地区：{draft.regionOverrides[node.id] ? '手动归类' : '名称推断'}</span></div>
                    </div>
                    <div className="network-node-actions">
                      <button type="button" disabled={blocked || dirty || !selectable} onClick={() => void run('network_select', { id: node.id, fixed: false }, '已选为当前节点，自动模式会在故障时使用备用')}>使用</button>
                      {advancedOpen && <><button type="button" disabled={blocked || dirty || !selectable} onClick={() => void run('network_select', { id: node.id, fixed: true }, '已固定节点，故障时等待恢复或手动选择')}>固定</button>
                      <button type="button" disabled={blocked || dirty || !status.settings.enabled} onClick={() => void run('network_check', { ids: [node.id] })}>检测</button>
                      <button type="button" disabled={blocked} onClick={() => { setEditNode(node); setEditRegion(effectiveRegion(node)); setEditError(''); }}>地区</button>
                      <label className="network-exclude"><input type="checkbox" checked={excluded} disabled={blocked} onChange={event => changeDraft({ ...draft, excludedNodeIds: event.target.checked ? [...draft.excludedNodeIds, node.id] : draft.excludedNodeIds.filter(id => id !== node.id) })} />排除</label>
                      <div className="network-order-actions"><button type="button" aria-label={`上移节点 ${node.name}`} disabled={blocked || index === 0} onClick={() => moveNode(node, -1)}>↑</button><button type="button" aria-label={`下移节点 ${node.name}`} disabled={blocked || index === localNodes.length - 1} onClick={() => moveNode(node, 1)}>↓</button></div></>}
                    </div>
                  </article>;
                })}</div>
              </details>;
            })}
          </div>
          <p className="network-health-note">基础检测不代表真实业务一定可用。</p>
        </section>
      </div>
      <div className="network-receipt-total"><span>当前可选节点</span><strong>{eligible.length}<small> / {status.nodes.length}</small></strong></div>
      {status.pendingUpdate && <p className="network-callout" role="status">新订阅已验证，正在等待在途请求完成后应用；当前请求继续使用原节点。</p>}
      {busy && <p className="network-operation" role="status"><span className="spinner" />{operation === 'network_check' ? '正在检测节点，其他板块可继续使用' : operation === 'network_import' ? '正在导入并验证订阅' : operation === 'network_refresh' ? '正在更新订阅' : '正在处理网络操作；如有在途请求，会等待其完成'}</p>}
      {status.error && <p className="network-callout network-danger" role="alert">{safeError(status.error)}</p>}
      {localError && !importOpen && !removing && <p className="network-callout network-danger" role="alert">{localError}</p>}
    </>}

    {importOpen && <Modal label="导入代理订阅" busy={busy} onClose={() => { clearImport(); setLocalError(''); setImportOpen(false); }}><div className="modal network-import-dialog"><h2>添加代理订阅</h2><p>添加 Clash / Mihomo 订阅链接或本地 YAML，每个订阅最多 8 MiB、1000 个节点。已有订阅保留；添加后选择需要的订阅，再启用代理。</p>
      <label>订阅名称（可选）<input value={name} maxLength={80} disabled={busy} onChange={event => setName(event.target.value)} autoComplete="off" /></label>
      <label>HTTPS 订阅链接<input ref={linkInput} type="password" value={link} disabled={busy} onChange={event => setLink(event.target.value)} autoComplete="new-password" spellCheck={false} placeholder="https://…" /></label>
      <div className="network-import-actions"><button type="button" className="network-primary" disabled={busy || !link.trim()} onClick={() => void importLink()}>导入链接</button><span>或</span><label className={'network-file-button' + (busy ? ' is-disabled' : '')}>选择本地 YAML<input ref={fileInput} type="file" accept=".yaml,.yml,text/yaml,application/yaml" disabled={busy} onChange={event => void importFile(event)} /></label></div>
      {localError && <p className="network-callout network-danger" role="alert">{localError}</p>}
      <div className="network-actions"><button type="button" disabled={busy} onClick={() => { clearImport(); setLocalError(''); setImportOpen(false); }}>取消</button></div>
    </div></Modal>}
    {removing && <Modal label="移除代理订阅" busy={busy} onClose={() => { setRemoving(null); setLocalError(''); }}><div className="modal small"><h2>移除 {removing.name}？</h2><p>{removing.id === status?.selectedSubscriptionId ? '这是当前选用的订阅。等待在途请求完成后关闭内置代理，恢复使用系统代理；其他订阅保留，需要时由你重新选用。' : '只移除此订阅及其节点，当前选用的订阅与连接保持。'}</p>{localError && <p className="network-callout network-danger" role="alert">{localError}</p>}<div className="network-actions"><button type="button" disabled={busy} onClick={() => { setRemoving(null); setLocalError(''); }}>取消</button><button type="button" className="network-danger-text" disabled={busy} onClick={() => void run('network_remove', { id: removing.id }, '代理订阅已移除').then(success => { if (success) setRemoving(null); })}>确认移除订阅</button></div></div></Modal>}
    {editNode && draft && <Modal label="修改节点地区" busy={false} onClose={() => setEditNode(null)}><div className="modal small"><h2>修改节点地区</h2><p className="network-node-edit-name">{editNode.name}</p><label>国家／地区<select aria-label="国家／地区" value={Object.hasOwn(regionNames, editRegion) ? editRegion : 'OTHER'} onChange={event => { setEditRegion(event.target.value === 'OTHER' ? '' : event.target.value); setEditError(''); }}>{Object.entries(regionNames).map(([code, label]) => <option key={code} value={code}>{label}{code !== 'UNKNOWN' ? ` · ${code}` : ''}</option>)}<option value="OTHER">其他地区</option></select></label>{!Object.hasOwn(regionNames, editRegion) && <label>两位地区代码<input maxLength={2} value={editRegion} onChange={event => { setEditRegion(event.target.value.toUpperCase()); setEditError(''); }} placeholder="例如 LU" /></label>}<p className="network-muted">原名称推断：{regionLabel(editNode.inferredRegion)}。修改后点击卡组中的“保存节点策略”生效。</p>{editError && <p className="network-danger-text" role="alert">{editError}</p>}<div className="network-actions"><button type="button" onClick={() => setEditNode(null)}>取消</button><button type="button" onClick={() => { changeDraft({ ...draft, regionOverrides: Object.fromEntries(Object.entries(draft.regionOverrides).filter(([id]) => id !== editNode.id)) }); setEditNode(null); }}>恢复名称推断</button><button type="button" className="network-primary" onClick={() => { if (!/^[A-Z]{2}$|^UNKNOWN$/.test(editRegion)) { setEditError('请选择地区，或填写两位地区代码'); return; } changeDraft({ ...draft, regionOverrides: { ...draft.regionOverrides, [editNode.id]: editRegion } }); setEditNode(null); }}>确定地区</button></div></div></Modal>}
  </div>;
}
