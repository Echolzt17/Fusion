import { useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { requestStatusName } from './notifications';
import { Modal } from './Modal';
import './error-report.css';
type Preview = { draftId: string; errorCount: number; summaryRequests?: number; summarySuccess?: number; summaryFailure?: number; summaryRecovered?: number; summaryPartial?: boolean; retryExisting?: boolean; rangeFrom: number; rangeTo: number; compressedBytes: number; historyLimited: boolean; destination: string; categories: Record<string,number> };
type Receipt = {reportId:string;receivedAt:number;acceptedErrors:number;duplicateErrors:number};
type ReportState = { phase: 'idle'|'empty'|'prepared'|'uploading'|'failed'|'sent'; preview: Preview|null; message: string|null; receipt: Receipt|null; history?:Receipt[]; historyError?:string|null };
const names: Record<string,string> = {upstream_error:'上游服务异常',network_error:'连接失败',upstream_header_timeout:'等待响应超时',upstream_read_timeout:'读取超时',stream_interrupted:'异常断流',upstream_rate_limited:'上游限流',response_failed:'上游响应失败',invalid_image_response:'图片响应异常',authorization_unavailable:'授权不可用',account_unavailable:'账号不可用',continuity_unavailable:'上下文不可用'};
const date = (n:number) => new Date(n*1000).toLocaleString('zh-CN',{hour12:false});
export function ErrorReport({disabled,notify}:{disabled:boolean;notify:(notice:{text:string;error:boolean})=>void}) {
  const [state,setState]=useState<ReportState|null>(null);
  const [open,setOpen]=useState(false);
  const [working,setWorking]=useState(false);
  const busy=useRef(false);
  const button=useRef<HTMLButtonElement>(null);
  const historyButton=useRef<HTMLButtonElement>(null);
  const [historyOpen,setHistoryOpen]=useState(false);
  const [history,setHistory]=useState<Receipt[]>([]);
  const [historyError,setHistoryError]=useState<string|null>(null);
  async function showHistory() {
    const result=await invoke<ReportState>('error_report_status');
    setHistory(result.history||[]);setHistoryError(result.historyError||null);setHistoryOpen(true);
  }
  async function action(run:()=>Promise<void>) {
    if(busy.current)return;busy.current=true;setWorking(true);
    try {await run();} catch {notify({text:'错误报告操作未完成，请重试；尚未确认上传成功',error:true});}
    finally {busy.current=false;setWorking(false);}
  }
  async function prepare(summaryOnly=false) {
    const result=await invoke<ReportState>('prepare_error_report',{summaryOnly});setState(result);
    if(result.phase==='empty'){notify({text:result.message||'暂无可上报错误',error:false});return;}
    if(result.preview)setOpen(true);
  }
  async function submit() {
    if(!state?.preview)return;
    const result=await invoke<ReportState>('submit_error_report',{draftId:state.preview.draftId});
    if(result.phase==='empty'){
      // Re-enable the opener before removing the modal so it can restore focus.
      // The action's busy ref stays guarded until its finally block completes.
      setWorking(false);setState(result);setOpen(false);
      notify({text:result.message||'暂无新增可上报诊断记录',error:false});return;
    }
    setState(result);
    if(result.phase==='sent')notify({text:`错误日志上报成功；新增 ${result.receipt?.acceptedErrors} 条，重复 ${result.receipt?.duplicateErrors} 条。可在“最近上报”查看。`,error:false});
  }
  return <>
    <button ref={button} className="error-report-trigger" disabled={disabled||working} onClick={()=>void action(()=>prepare())}>
      <span>{working&&!open?'正在准备':'日志上报'}</span><span className="report-clip" aria-hidden="true">{['lt','rt','rb','lb'].map(c=><i className={'report-corner '+c} key={c}/>)}</span><i className="report-arrow left" aria-hidden="true"/><i className="report-arrow right" aria-hidden="true"/>
    </button>
    <button ref={historyButton} className="report-history-trigger" disabled={working} onClick={()=>void action(showHistory)}>最近上报</button>
    {historyOpen&&<Modal label="最近上报" busy={working} returnFocus={historyButton} onClose={()=>setHistoryOpen(false)}>
      <div className="error-report-card">
        <h2>最近上报</h2><p className="report-intro">保留最近 10 次成功回执。维护人员可直接检索已上传报告，无需提供编号。</p>
        {historyError&&<p role="alert">{historyError}</p>}
        {history.length===0&&!historyError&&<p>暂无已保存的上报记录。旧版未保存的回执仍可由维护人员在服务器查询。</p>}
        <ol className="report-history">{history.map(receipt=><li key={receipt.reportId}>
          <strong>上报成功</strong><time>{date(receipt.receivedAt)}</time>
          <p>{receipt.acceptedErrors+receipt.duplicateErrors} 条错误 · 新增 {receipt.acceptedErrors} · 重复 {receipt.duplicateErrors}</p>
          <details><summary>查看回执</summary><p className="report-receipt">{receipt.reportId}</p>
            <button onClick={()=>void action(async()=>{try {await navigator.clipboard.writeText(receipt.reportId);notify({text:'报告编号已复制',error:false});} catch {notify({text:'无法复制，可选中编号手动复制',error:true});}})}>复制编号</button>
          </details>
        </li>)}</ol>
        <div className="report-actions"><button onClick={()=>setHistoryOpen(false)}>完成</button></div>
      </div>
    </Modal>}
    {open&&state?.preview&&<Modal label="确认上报错误日志" busy={working} returnFocus={button} onClose={()=>setOpen(false)}>
      <div className="error-report-card">
        <div className="report-symbol" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"><path d="M12 8v5m0 3h.01M3 18 10 5a2.2 2.2 0 0 1 4 0l7 13a2 2 0 0 1-2 3H5a2 2 0 0 1-2-3Z"/></svg></div>
        <h2>{state.phase==='sent'?'错误日志已上报':'上报错误日志'}</h2>
        <p className="report-intro">发送新增失败记录与变化的数值统计。确认后会排除服务器已收录的内容。</p>
        {state.phase==='prepared'&&!state.preview.retryExisting&&(state.preview.summaryRequests??0)>0&&<label className="report-summary-only"><input type="checkbox" checked={state.preview.errorCount===0} disabled={working} onChange={e=>void action(()=>prepare(e.target.checked))}/>仅上报统计摘要</label>}
        {state.preview.retryExisting&&<p>继续重试原报告，内容与上次上传保持一致。</p>}
        <dl className="report-facts"><dt>错误记录</dt><dd>{state.preview.errorCount} 条</dd><dt>统计覆盖请求</dt><dd>{state.preview.summaryRequests ?? 0} 条{state.preview.summaryPartial ? " · 覆盖不完整" : ""}</dd><dt>筛选时间</dt><dd>{date(state.preview.rangeFrom)} 至 {date(state.preview.rangeTo)}</dd><dt>压缩大小</dt><dd>{(state.preview.compressedBytes/1024).toFixed(1)} KiB</dd><dt>接收位置</dt><dd>{new URL(state.preview.destination).host}</dd></dl>
        <div className="report-categories">{Object.entries(state.preview.categories).map(([code,n])=><span key={code}>{names[code]||requestStatusName(code)} × {n}</span>)}</div>
        {state.preview.summarySuccess!==undefined&&<p>统计成功 {state.preview.summarySuccess} · 失败 {state.preview.summaryFailure} · 发送前恢复后完成 {state.preview.summaryRecovered}。取消和停止单独计数，不计作成功。</p>}
        <p className="report-privacy">不含逐条成功、取消或停止记录；摘要只包含计数与耗时分布。不含对话内容、图片、账号身份、节点地址或授权信息。服务器错误明细保留 30 天。</p>
        {state.preview.historyLimited&&<p className="report-privacy">本机明细保留量已达上限，更早的错误可能已清理，无法补回。</p>}
        {state.message&&<p role={state.phase==='failed'?'alert':'status'}>{state.message}</p>}
        {state.receipt&&<p className="report-receipt">报告编号：{state.receipt.reportId}</p>}
        <div className="report-actions">
          {state.phase!=='sent'&&<button className="report-confirm" disabled={working||disabled} onClick={()=>void action(submit)}>{working?'正在上报…':state.phase==='failed'?'重试同一报告':'确认上报'}</button>}
          <button className="report-cancel" disabled={working} onClick={()=>setOpen(false)}>{state.phase==='sent'?'完成':'取消'}</button>
          {state.phase==='failed'&&<button className="report-save" disabled={working} onClick={()=>void action(async()=>{const path=await invoke<string>('save_error_report',{draftId:state.preview!.draftId});notify({text:`脱敏错误包已保存：${path}`,error:false});})}>保存脱敏错误包</button>}
        </div>
      </div>
    </Modal>}
  </>;
}
