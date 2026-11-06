import { useEffect, useRef, useState } from 'react';
import { ActionButton } from './Controls';
import type { Tokens } from './PoolTelemetry';
import './usage-dashboard.css';

export type DashboardUsage = {
  tokens: Tokens; measured: boolean;
  days: { date: string; totalTokens: number | null }[];
  hours: { hour: number; minutes: number | null; provisional: boolean }[];
  today: string; timezone: string; updatedAt: number | null;
  scanning: boolean; historyPartial: boolean; activityPartial: boolean; error: string | null;
};
const compact = (n: number) => n >= 1e6 ? `${(n / 1e6).toFixed(2)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}K` : n.toLocaleString('zh-CN');
const hourLabel = (hour: number) => `${String(hour).padStart(2, '0')}:00`;
const minutesLabel = (minutes: number) => `${minutes.toLocaleString('zh-CN', { maximumFractionDigits: 1 })} 分钟`;
function path(points: { x: number; y: number | null }[]) {
  let connected = false;
  return points.map(p => { if (p.y === null) { connected = false; return ''; } const command = connected ? 'L' : 'M'; connected = true; return `${command}${p.x},${p.y}`; }).join(' ');
}

export function UsageDashboard({ data, onDetails }: { data: DashboardUsage | null; onDetails: () => void }) {
  const panel = useRef<HTMLElement>(null);
  const [width, setWidth] = useState(720);
  useEffect(() => {
    if (!panel.current) return;
    const observer = new ResizeObserver(entries => setWidth(Math.max(280, Math.floor(entries[0].contentRect.width - 20))));
    observer.observe(panel.current);
    return () => observer.disconnect();
  }, []);
  const [mode, setMode] = useState<'bar' | 'line'>(() => { try { return localStorage.getItem('pool-usage-chart') === 'line' ? 'line' : 'bar'; } catch { return 'bar'; } });
  const [hoverHour, setHoverHour] = useState<number | null>(null);
  const switchMode = () => setMode(old => { const next = old === 'bar' ? 'line' : 'bar'; try { localStorage.setItem('pool-usage-chart', next); } catch {} return next; });
  const tokens = data?.tokens;
  const measured = !!data?.measured;
  const days = data?.days ?? [];
  const max = Math.max(1, ...days.map(d => d.totalTokens ?? 0));
  const hours = data?.hours ?? [];
  const hasHours = hours.some(h => h.minutes !== null);
  const dayPoints = days.map((d, i) => ({ x: 40 + i * ((width - 80) / 6), y: d.totalTokens === null ? null : 118 - d.totalTokens / max * 88 }));
  const hourPoints = hours.map(h => ({ x: 42 + h.hour * ((width - 60) / 23), y: h.minutes === null ? null : 139 - h.minutes / 60 * 120 }));
  const labelStep = width >= 1000 ? 1 : width >= 650 ? 2 : 3;
  const active = hoverHour === null ? null : hours.find(h => h.hour === hoverHour);
  const hoursTotal = hours.reduce((sum, h) => sum + (h.minutes ?? 0), 0);
  const hourDescription = (h: DashboardUsage['hours'][number]) => `${hourLabel(h.hour)}—${hourLabel(h.hour + 1)}：${h.minutes === null ? '尚无数据' : minutesLabel(h.minutes)}${h.provisional ? '（暂计）' : ''}`;
  const dayDescription = days.map(d => `${d.date}：${d.totalTokens === null ? '暂无明细' : d.totalTokens.toLocaleString('zh-CN') + ' Token'}`).join('；');
  const chartHint = `近 7 天${mode === 'bar' ? '柱状图' : '折线图'}，点击切换为${mode === 'bar' ? '折线图' : '柱状图'}`;
  return <section ref={panel} className="usage-panel usage-dashboard" aria-label="本机 Codex 用量">
    <div className="analytics-total"><span className="analytics-icon"><svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true"><path d="M2 12h4l3-8 6 16 3-8h4" /></svg></span><h2>Token 用量</h2><span className="badge neutral" title="账号池累计与本机非池用量合计">{measured ? '累计 ' + compact(tokens!.totalTokens) : data?.scanning || !data ? '读取中' : '暂无用量'}</span></div>
    <div className="token-metrics"><div><small>输入 Token</small><strong>{measured ? compact(tokens!.inputTokens) : '—'}</strong></div><div><small>输出 Token</small><strong>{measured ? compact(tokens!.outputTokens) : '—'}</strong></div></div>
    <div className="usage-chart-heading"><h3>今日使用时段</h3><span title="任务运行时长含思考和工具执行，并发重叠只算一次；未结束任务计至最后可确认的活动记录。">{hasHours ? `已记录 ${minutesLabel(hoursTotal)}${data?.activityPartial ? ' · 暂计' : ''}` : !data || data.scanning ? '正在读取' : '暂无时长记录'}</span></div>
    <div className="usage-hours">
      <svg viewBox={`0 0 ${width} 172`} role="img" aria-label={`今日每小时任务运行分钟数。${hours.map(hourDescription).join('；')}`}>
        {[0, 10, 20, 30, 40, 50, 60].map(n => <g key={n}><line className="usage-gridline" x1="42" x2={width - 18} y1={139 - n * 2} y2={139 - n * 2} /><text className="usage-axis" x="32" y={143 - n * 2} textAnchor="end">{n}</text></g>)}
        <path className="usage-line" d={path(hourPoints)} />
        {hours.map((h, i) => <g key={h.hour}>
          <text className={`usage-axis usage-hour-label ${(h.hour % labelStep === 0 && (labelStep === 1 || h.hour < 22)) || h.hour === 23 ? 'major' : ''}`} x={hourPoints[i].x} y="160" textAnchor="middle">{hourLabel(h.hour)}</text>
          {h.minutes !== null && <><circle className={`usage-point ${h.provisional ? 'provisional' : ''}`} cx={hourPoints[i].x} cy={hourPoints[i].y!} r="3" /><rect className="usage-hit" x={hourPoints[i].x - 13} y="10" width="26" height="132" tabIndex={0} role="img" aria-label={hourDescription(h)} onMouseEnter={() => setHoverHour(h.hour)} onMouseLeave={() => setHoverHour(null)} onFocus={() => setHoverHour(h.hour)} onBlur={() => setHoverHour(null)}><title>{hourDescription(h)}</title></rect></>}
        </g>)}
      </svg>
      <div className="usage-hour-detail" aria-live="polite">{active ? hourDescription(active) : `${data?.today || '今天'} · ${data?.timezone || '本地时间'}`}</div>
    </div>
    <div className="usage-chart-heading"><h3>近 7 天</h3><span>{mode === 'bar' ? '柱状' : '折线'} · 点击图表切换</span></div>
    <button type="button" className="usage-trend" aria-label={chartHint} aria-describedby="usage-trend-values" onClick={switchMode} data-mode={mode}>
      {mode === 'bar' ? <div className="chart">{days.map(d => <div className="chart-column" key={d.date} title={`${d.date}：${d.totalTokens === null ? '暂无明细' : d.totalTokens.toLocaleString('zh-CN') + ' Token'}`}><span>{d.totalTokens === null ? '—' : compact(d.totalTokens)}</span><div className="bar-track"><i style={{ height: `${(d.totalTokens ?? 0) / max * 100}%` }} /></div><small>{d.date.slice(5).replace('-', '/')}</small></div>)}</div> : <svg className="usage-week-line" viewBox={`0 0 ${width} 151`} aria-hidden="true">
        <line className="usage-gridline" x1="25" x2={width - 25} y1="118" y2="118" />
        <path className="usage-line" d={path(dayPoints)} />
        {days.map((d, i) => <g key={d.date}><title>{d.date}：{d.totalTokens === null ? '暂无明细' : d.totalTokens.toLocaleString('zh-CN') + ' Token'}</title><text className="usage-value" x={dayPoints[i].x} y={dayPoints[i].y === null ? 23 : dayPoints[i].y! - 12} textAnchor="middle">{d.totalTokens === null ? '—' : compact(d.totalTokens)}</text>{dayPoints[i].y !== null && <circle className="usage-point" cx={dayPoints[i].x} cy={dayPoints[i].y!} r="4" />}<text className="usage-axis" x={dayPoints[i].x} y="143" textAnchor="middle">{d.date.slice(5).replace('-', '/')}</text></g>)}
      </svg>}
    </button>
    <span id="usage-trend-values" className="usage-sr-only">{dayDescription || '暂无明细'}</span>
    <div className="usage-details"><span><small>已知缓存输入</small><b>{tokens?.cachedTokens == null ? '—' : compact(tokens.cachedTokens)}</b></span><span><small>已知推理输出</small><b>{tokens?.reasoningTokens == null ? '—' : compact(tokens.reasoningTokens)}</b></span></div>
    <div className="analytics-footer"><span title={data?.updatedAt ? `最后读取 ${new Date(data.updatedAt).toLocaleString('zh-CN', { hour12: false })}。缓存包含在输入中，推理包含在输出中。` : undefined}>本机总用量 · {data?.timezone || '本地时间'}{data?.scanning && ' · 补读中'}{data?.historyPartial && <small className="usage-coverage" title="统计本机仍可读取的记录；部分旧记录缺失或无法还原日期，累计与近 7 天明细的覆盖范围可能不同。">部分历史缺少明细</small>}{data?.error && <small className="usage-coverage" role="status">{data.error}</small>}</span><ActionButton kind="next" onClick={onDetails}>账号池明细</ActionButton></div>
  </section>;
}
