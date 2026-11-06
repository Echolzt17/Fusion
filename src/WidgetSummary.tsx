// Adapted from SSpisso/kind-cobra-45 (MIT); see THIRD_PARTY_NOTICES.md.
export type RequestSummary = {
  requests: number; measuredRequests: number; totalTokens: number;
  success: number; failed: number; other: number; unclassifiedRequests: number;
  historyPartial: boolean; models: { name: string; requested: boolean; requests: number }[];
  operations: Record<string, number>;
};
export type WidgetDetails = {
  conversationKey: string | null; current: RequestSummary | null; all: RequestSummary;
  activeCurrent: number; activeAll: number;
};
const number = (value: number) => value.toLocaleString('zh-CN');
const operations = [['responses', '对话'], ['image_generate', '图片生成'], ['image_edit', '图片编辑'], ['compact', '上下文压缩']] as const;

export function WidgetSummary({ details, scope, onScopeChange, warning, onOpenConsole }: {
  details?: WidgetDetails; scope: 'current' | 'all'; onScopeChange: (scope: 'current' | 'all') => void;
  warning?: string | null; onOpenConsole: () => void;
}) {
  const summary = scope === 'all' ? details?.all : details?.current;
  const active = scope === 'all' ? details?.activeAll : details?.activeCurrent;
  const partial = summary && (summary.measuredRequests < summary.requests || summary.historyPartial);
  const models = [...(summary?.models ?? [])].sort((a, b) => b.requests - a.requests || a.name.localeCompare(b.name));
  return <>
    <header className="widget-summary-header"><strong>请求概览</strong><span>Fusion</span></header>
    <div className="widget-scope" role="tablist" aria-label="汇总范围" onKeyDown={e => {
      if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
      e.preventDefault();
      const next = e.key === 'Home' ? 'current' : e.key === 'End' ? 'all' : scope === 'current' ? 'all' : 'current';
      onScopeChange(next);
      e.currentTarget.querySelector<HTMLButtonElement>(`[data-scope="${next}"]`)?.focus();
    }}>{(['current', 'all'] as const).map(value => <button key={value} role="tab" id={'widget-tab-' + value} data-scope={value}
      aria-controls="widget-summary-body" aria-selected={scope === value} tabIndex={scope === value ? 0 : -1}
      onClick={() => onScopeChange(value)}>{value === 'current' ? '本对话' : '全部对话'}</button>)}</div>
    <div className="widget-summary-body" id="widget-summary-body" role="tabpanel" aria-labelledby={'widget-tab-' + scope} tabIndex={0}>
      <p className="widget-scope-note">{scope === 'all' ? '所有经过账号池的业务请求' : '跟随最近请求对话'}{scope === 'current' && details?.conversationKey && <span>{details.conversationKey.startsWith('thread:') ? 'T' : 'S'}-{details.conversationKey.split(':').at(-1)?.slice(0, 12)}</span>}</p>
      {warning && <p className="widget-warning" role="alert">{warning}</p>}
      {!details ? <p className="widget-empty">正在读取汇总…</p> : !summary ? <p className="widget-empty">最近请求未识别对话，暂不显示本对话汇总。</p> : <>
        <section className="widget-summary-section">
          <h2>请求结果{active ? <span className="widget-active" role="status">进行中 {number(active)}</span> : null}</h2>
          <div className="widget-request-total"><span>已记录请求</span><strong>{number(summary.requests)}<small> 次</small></strong></div>
          <div className="widget-results"><span className="widget-success">成功 <b>{number(summary.success)}</b></span><span className="widget-failed">失败 <b>{number(summary.failed)}</b></span><span title="取消、服务停止及执行状态未确认的请求">其他 <b>{number(summary.other)}</b></span></div>
          {summary.unclassifiedRequests > 0 && <p className="widget-history-note">历史未记录分类 {number(summary.unclassifiedRequests)} 次</p>}
        </section>
        <section className="widget-summary-section">
          <h2>使用模型</h2>
          {models.length === 0 && <p className="widget-empty">暂无模型记录</p>}
          <dl className="widget-summary-rows">{models.map(model => <div key={(model.requested ? 'requested:' : 'response:') + model.name}><dt title={model.name || '未提供模型'}>{model.name || '未提供模型'}{model.requested && <small>请求模型</small>}</dt><dd>{number(model.requests)} 次</dd></div>)}</dl>
          {summary.unclassifiedRequests > 0 && <div className="widget-unknown-row"><span>历史未记录</span><span>{number(summary.unclassifiedRequests)} 次</span></div>}
        </section>
        <section className="widget-summary-section">
          <h2>请求类型</h2>
          <dl className="widget-summary-rows">{operations.filter(([key]) => summary.operations[key] > 0).map(([key, label]) => <div key={key}><dt>{label}</dt><dd>{number(summary.operations[key])} 次</dd></div>)}</dl>
          {summary.unclassifiedRequests > 0 && <div className="widget-unknown-row"><span>历史未记录</span><span>{number(summary.unclassifiedRequests)} 次</span></div>}
          {summary.requests === 0 && <p className="widget-empty">尚无已记录请求</p>}
        </section>
        {summary.historyPartial && <p className="widget-history-note">历史覆盖不完整，仅汇总可证实的数据。</p>}
      </>}
    </div>
    <footer className="widget-summary-footer"><div><span>总 Token{partial ? ' *' : ''}</span><strong>{summary?.measuredRequests ? number(summary.totalTokens) : '—'}</strong>{partial && <small>仅含已上报用量</small>}</div><button onClick={onOpenConsole}>打开控制台 <span aria-hidden="true">↗</span></button></footer>
  </>;
}
