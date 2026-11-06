import { useEffect, useRef, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { LogDiagnostic, type Account, type Log, type Tokens } from './PoolTelemetry';
import { requestStatusName } from './notifications';
import { createReadCoordinator } from './refresh';
import { RecordSelector } from './RecordSelector';
import { RecordBranch, RecordCategories } from './RequestNavigation';
import './request-records.css';

export type ConversationUsage = { conversationKey: string; requests: number; measuredRequests: number; tokens: Tokens; firstObservedAt: number; lastStartedAt: number; lastAccount: string | null; historyPartial: boolean };
export type RecentRequest = { id: string; conversationKey: string | null; startedAt: number; account: string | null };
type Branch = { conversationKey: string | null; operation: string; requests: number };
type Facets = { branches: Branch[]; accounts: (string | null)[]; models: string[]; unknownModel: boolean };
export type HistoryIndex = { conversations: ConversationUsage[]; recentRequest?: RecentRequest | null; catalog?: { requests: Facets; system: Facets } };
type Cursor = { at: number; row: number };
type LogPage = { items: Log[]; nextCursor: Cursor | null };
type Filters = { result: string; operation: string; account: string; model: string };
const emptyFilters: Filters = { result: 'all', operation: 'all', account: 'all', model: 'all' };
const emptyFacets: Facets = { branches: [], accounts: [], models: [], unknownModel: false };
export const conversationName = (key?: string | null) => key ? '对话 ' + (key.startsWith('thread:') ? 'T' : 'S') + '-' + key.split(':').at(-1)!.slice(0,12) : '未识别对话';
export const modelName = (log: Log) => log.responseModel
  ? log.requestedModel && log.requestedModel !== log.responseModel ? '返回模型 ' + log.responseModel + ' · 请求模型 ' + log.requestedModel : '模型 ' + log.responseModel
  : '请求模型 ' + (log.requestedModel || '未知') + ' · 返回模型未提供';
const number = (n: number) => n.toLocaleString('zh-CN');
const date = (n: number) => new Date(n).toLocaleString('zh-CN', { hour12: false });
const operationName: Record<string,string> = { responses:'对话请求', compact:'上下文压缩', images:'图片任务', image_generate:'图片生成', image_edit:'图片编辑', models:'模型目录检查' };
const resultOptions = [{ value: 'all', label: '全部结果' }, { value: 'success', label: '成功' }, { value: 'other', label: '其他结果', detail: '失败 / 取消 / 停止 / 未确认' }];
const operations = (kind: string) => kind === 'all' ? [] : kind === 'images' ? ['image_generate','image_edit'] : [kind];

export function RequestRecords({ accounts, index, indexError, selection: controlled, onSelectionChange, revision, active = true }: { accounts: Account[]; index: HistoryIndex; indexError?: boolean; selection?: string; onSelectionChange?: (value: string) => void; revision?: unknown; active?: boolean }) {
  const [system, setSystem] = useState(false);
  const [localSelection, setLocalSelection] = useState('auto');
  const selection = controlled ?? localSelection;
  const [scope, setScope] = useState('all');
  const [kind, setKind] = useState('all');
  const [filters, setFilters] = useState<Filters>(emptyFilters);
  const [navigationOpen, setNavigationOpen] = useState(false);
  const navigationTrigger = useRef<HTMLButtonElement>(null);
  const [cursors, setCursors] = useState<(Cursor|null)[]>([null]);
  const [page, setPage] = useState<LogPage>({ items: [], nextCursor: null });
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const signature = useRef('');
  const reader = useRef<ReturnType<typeof createReadCoordinator> | null>(null);
  if (!reader.current) reader.current = createReadCoordinator((_name,error)=>{setError(error ? '请求记录读取失败或超时，保留上次数据' : '');setLoading(false);});
  useEffect(()=>()=>reader.current?.dispose(),[]);
  const recentKey = index.recentRequest ? index.recentRequest.conversationKey : index.conversations[0]?.conversationKey;
  const selectedKey = scope === 'auto' ? recentKey : scope === 'conversation' ? selection : null;
  const facets = (system ? index.catalog?.system : index.catalog?.requests) ?? emptyFacets;
  const summary = !system && selectedKey ? index.conversations.find(c=>c.conversationKey===selectedKey) : undefined;
  const cursor = cursors.at(-1) ?? null;
  const query = JSON.stringify([system, scope, selectedKey, kind, filters]);
  const queryRef = useRef(query);
  useEffect(()=>{
    if (!active) { reader.current!.invalidate(); setLoading(false); return; }
    if (queryRef.current !== query) { reader.current!.invalidate(); queryRef.current = query; if (cursors.length > 1) { setCursors([null]); return; } }
    const nextSignature = JSON.stringify([query,cursor]);
    if (signature.current !== nextSignature) { reader.current!.invalidate(); signature.current = nextSignature; setError(''); setPage({ items: [], nextCursor: null }); }
    setLoading(true);
    const byNode = operations(kind), byFilter = operations(filters.operation);
    const matched = byNode.length && byFilter.length ? byNode.filter(op=>byFilter.includes(op)) : byNode.length ? byNode : byFilter;
    if (byNode.length && byFilter.length && !matched.length) { setPage({items:[],nextCursor:null});setLoading(false);return; }
    reader.current!.refresh([{ name:'请求记录', read:()=>invoke<LogPage>('request_history', {
      conversationKey: system ? null : selectedKey ?? null, result:filters.result, system, cursor,
      filter:{ unknownConversation: !system && (scope === 'unknown' || (scope === 'auto' && !selectedKey)), operations:matched,
        account:filters.account === 'all' ? null : filters.account.slice(1), model:filters.model === 'all' ? null : filters.model.slice(1) }
    }), update:setPage }]);
  },[query,cursor,index,revision,active]);
  function dismissNavigation() { setNavigationOpen(false); navigationTrigger.current?.focus(); }
  function choose(nextScope: string, key?: string, nextKind = 'all') {
    setScope(nextScope); setKind(nextKind); setCursors([null]);
    if (key && nextScope === 'conversation') { setLocalSelection(key); onSelectionChange?.(key); }
  }
  function setCategory(next: boolean) { setSystem(next); setKind('all'); setScope('all'); setFilters(emptyFilters); setCursors([null]); }
  function filter(name: keyof Filters, value: string) { setFilters(f=>({...f,[name]:value}));setCursors([null]); }
  const accountName = (id: string | null) => {
    if (!id) return '账号未分配';
    const account = accounts.find(a=>a.id===id);
    return account ? account.email.trim() || '账号邮箱未知' : '账号已移除';
  };
  const branchesFor = (key: string | null) => facets.branches.filter(b=>b.conversationKey===key);
  const count = (branches: Branch[], type = 'all') => branches.filter(b=>type==='all'||operations(type).includes(b.operation)).reduce((n,b)=>n+b.requests,0);
  const currentBranches = scope === 'all' || system ? facets.branches : branchesFor(selectedKey ?? null);
  const retained = count(currentBranches,kind);
  const conversationKeys = [...new Set([...index.conversations.map(c=>c.conversationKey), ...facets.branches.map(b=>b.conversationKey).filter((k): k is string=>!!k)])];
  function categoryNodes(branches: Branch[], nodeScope: string, key?: string) {
    const node = (type: string, children?: ReactNode) => <RecordBranch key={type} label={operationName[type]} count={count(branches,type)} selected={scope===nodeScope && (nodeScope!=='conversation'||selection===key) && kind===type} onSelect={()=>choose(nodeScope,key,type)}>{children}</RecordBranch>;
    return <>{count(branches,'responses')>0 && node('responses')}{count(branches,'images')>0 && node('images',<>{count(branches,'image_generate')>0 && node('image_generate')}{count(branches,'image_edit')>0 && node('image_edit')}</>)}{count(branches,'compact')>0 && node('compact')}</>;
  }
  return <div className="request-records" onKeyDown={e=>{if(e.key==='Escape' && navigationOpen){e.preventDefault();e.stopPropagation();dismissNavigation();}}}>
    <button ref={navigationTrigger} className="record-navigation-trigger" aria-expanded={navigationOpen} onClick={()=>setNavigationOpen(!navigationOpen)}>分类导航 <span>{system?'2 系统检查':'1 全部对话'}</span></button>
    <div className="records-layout">
      <nav className={'record-navigation'+(navigationOpen?' is-open':'')} aria-label="请求分类导航">
        <RecordCategories system={system} onChange={setCategory}/>
        <div className="record-tree" aria-label="对话与请求类别"><ul>
          {system ? <RecordBranch label="模型目录检查" count={count(facets.branches)} selected onSelect={()=>setKind('models')}/> : <RecordBranch label="全部对话" count={count(facets.branches)} selected={scope==='all' && kind==='all'} onSelect={()=>choose('all')} initialOpen>
            {conversationKeys.map(key=><RecordBranch key={key} label={conversationName(key)} count={count(branchesFor(key))} selected={(scope==='conversation' && selection===key || scope==='auto' && selectedKey===key) && kind==='all'} onSelect={()=>choose('conversation',key)}>{categoryNodes(branchesFor(key),'conversation',key)}</RecordBranch>)}
            {branchesFor(null).length>0 && <RecordBranch label="未识别对话" count={count(branchesFor(null))} selected={scope==='unknown' && kind==='all'} onSelect={()=>choose('unknown')}>{categoryNodes(branchesFor(null),'unknown')}</RecordBranch>}
          </RecordBranch>}
        </ul></div>
        {!system && <div className="conversation-location"><span>对话定位</span><button aria-pressed={scope==='auto'} onClick={()=>{choose('auto');setLocalSelection('auto');onSelectionChange?.('auto');}}>跟随最近请求</button>{scope==='auto' && <button disabled={!selectedKey} onClick={()=>choose('conversation',selectedKey ?? undefined,kind)}>固定查看</button>}<small>最近请求对话不代表 Codex 当前打开的聊天。</small></div>}
      </nav>
      <div className="record-content">
        <nav className="record-path" aria-label="当前位置"><span>请求记录</span><i>/</i><button onClick={()=>choose('all')}>{system?'系统检查':'全部对话'}</button>{!system && scope!=='all' && <><i>/</i><button onClick={()=>choose(scope,selectedKey ?? undefined)}>{conversationName(selectedKey)}</button></>}{kind!=='all' && <><i>/</i>{['image_generate','image_edit'].includes(kind) && <><button onClick={()=>setKind('images')}>图片任务</button><i>/</i></>}<span>{operationName[kind]}</span></>}</nav>
        <div className="request-filters">
          <RecordSelector label="筛选请求结果" value={filters.result} onChange={v=>filter('result',v)} options={resultOptions}/>
          <RecordSelector label="筛选请求类型" value={filters.operation} onChange={v=>filter('operation',v)} options={[{value:'all',label:'全部类型'},...Object.keys(operationName).filter(op=>op!=='images' && facets.branches.some(b=>b.operation===op)).map(op=>({value:op,label:operationName[op]}))]}/>
          <RecordSelector label="筛选账号" value={filters.account} onChange={v=>filter('account',v)} options={[{value:'all',label:'全部账号'},...facets.accounts.map(id=>({value:':'+(id??''),label:accountName(id)}))]}/>
          <RecordSelector label="筛选模型" value={filters.model} onChange={v=>filter('model',v)} options={[{value:'all',label:'全部模型'},...facets.models.map(model=>({value:':'+model,label:model})),...(facets.unknownModel?[{value:':',label:'模型未知'}]:[])]}/>
        </div>
        {Object.values(filters).some(v=>v!=='all') && <div className="record-filter-state"><span>当前筛选：{[resultOptions.find(o=>o.value===filters.result)?.label,filters.operation!=='all'?operationName[filters.operation]:'全部类型',filters.account!=='all'?accountName(filters.account.slice(1)||null):'全部账号',filters.model!=='all'?filters.model.slice(1)||'模型未知':'全部模型'].join(' · ')}</span><button onClick={()=>{setFilters(emptyFilters);setCursors([null]);}}>清除筛选</button></div>}
        {summary && <div className="conversation-summary"><strong>{conversationName(selectedKey)} · {scope==='auto'?'跟随最近请求':'固定查看'}</strong><span>长期累计 {number(summary.requests)} 次请求 · {summary.measuredRequests ? number(summary.tokens.totalTokens) : '未知'} Token</span><small>输入 {summary.measuredRequests ? number(summary.tokens.inputTokens) : '未知'} · 输出 {summary.measuredRequests ? number(summary.tokens.outputTokens) : '未知'} · 用量上报 {summary.measuredRequests}/{summary.requests}</small><small>{summary.historyPartial?'历史仅覆盖升级时保留的明细及之后请求':'采集起点'} · {date(summary.firstObservedAt)}</small>{count(branchesFor(selectedKey ?? null))===0 && <small>累计仍保留，明细已清理</small>}</div>}
        <div className="request-page-info"><span>第 {cursors.length} 页 · 每页 50 条</span><span>当前分类保留 {retained} 条{kind!=='all'?' · '+operationName[kind]:''}</span></div>
        {indexError && <p role="alert">对话目录读取失败或超时，保留上次目录与累计</p>}
        {error && <p role="alert">{error}</p>}
        {page.items.length===0 && <p className="pool-empty">{loading?'正在读取请求记录…':'当前范围与筛选下暂无记录'}</p>}
        <div className="record-list" aria-busy={loading}>{page.items.map(log=><RequestDetail key={log.id} log={log} account={accountName(log.account)} showConversation={!system && scope==='all'} system={system}/>)}</div>
        <div className="request-pagination"><button disabled={cursors.length===1||loading} onClick={()=>setCursors(c=>c.slice(0,-1))}>上一页</button><span>明细保留最近 2000 条<br/>长期累计不随清理减少</span><button disabled={!page.nextCursor||loading} onClick={()=>setCursors(c=>[...c,page.nextCursor])}>下一页</button></div>
        <p className="pool-note">{system?'仅显示本机模型目录检查及诊断。':'仅统计经过账号池的请求；缺失用量不计为零。模型筛选匹配请求模型或返回模型。缓存包含在输入中，推理包含在输出中。'}</p>
      </div>
    </div>
  </div>;
}

function RequestDetail({ log, account, showConversation, system }: { log: Log; account: string; showConversation: boolean; system: boolean }) {
  return <article className="log">
    <div className="log-top"><strong>{operationName[log.operation]||log.operation}</strong><span className={'badge '+(log.status==='success'?'':['cancelled','service_stopped'].includes(log.status)?'neutral':'warning')}>{log.status==='success'?'成功':requestStatusName(log.status)}</span></div>
    {!system && <><div className="request-identity"><span className="request-model">{modelName(log)}</span><span>{account}</span>{showConversation && <strong>{conversationName(log.conversationKey)}</strong>}</div><div className="log-usage">{log.usage ? <><span className="request-token-total">总计 <b>{number(log.usage.totalTokens)}</b><small>Token</small></span><span>输入 <b>{number(log.usage.inputTokens)}</b></span><span>输出 <b>{number(log.usage.outputTokens)}</b></span></> : '用量未知 · 上游未提供完整 usage'}</div></>}
    <div className="record-time"><time>{log.startedAt != null ? date(log.startedAt) : '开始时间未知'}</time><span>{number(log.elapsedMs)} ms</span></div>
    <div className="record-detail-groups"><details className="log-details"><summary>基本信息</summary><dl><dt>执行结果</dt><dd>{log.status==='success'?'成功':requestStatusName(log.status)}</dd><dt>开始时间</dt><dd>{log.startedAt != null ? date(log.startedAt) : '未知'}</dd><dt>记录时间</dt><dd>{date(log.at*1000)}</dd><dt>耗时</dt><dd>{number(log.elapsedMs)} ms</dd>{!system && <><dt>请求模型</dt><dd>{log.requestedModel || '未知'}</dd><dt>返回模型</dt><dd>{log.responseModel || '未提供'}</dd><dt>使用账号</dt><dd>{account}</dd></>}</dl></details>
    {!system && <details className="log-details"><summary>用量信息</summary><dl>{[['总 Token',log.usage?.totalTokens],['输入 Token',log.usage?.inputTokens],['输出 Token',log.usage?.outputTokens],['缓存输入',log.usage?.cachedTokens],['推理输出',log.usage?.reasoningTokens]].map(([label,value])=><div className="record-field" key={label}><dt>{label}</dt><dd>{typeof value==='number'?number(value):'未提供'}</dd></div>)}</dl><p className="subtle">{!log.usage?'用量未知':log.usage.cachedTokens==null || log.usage.reasoningTokens==null?'基础用量已提供，缓存或推理明细未完整提供':'已提供当前采集的全部用量字段'}。缓存属于输入，推理属于输出，不重复相加。</p></details>}
    <details className="log-details"><summary>诊断详情</summary>{log.advice && <p>{log.advice}</p>}<LogDiagnostic log={log}/><small>请求 {log.id}</small></details></div>
  </article>;
}
