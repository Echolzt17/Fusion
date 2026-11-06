import { test, expect } from '@playwright/test';
import { createNotificationStore } from '../../src/notifications';
import type { Log } from '../../src/PoolTelemetry';

test('transport failures across chats share one notice with every reference and no repeated unread alert', () => {
  let items:any[]=[]; const inbox=createNotificationStore(v=>items=v);const at=Math.ceil(Date.now()/1000);
  const make=(id:string,status='network_error',origin='transport'):Log=>({id,at,account:null,operation:'responses',conversationKey:`thread:${id}`,status,elapsedMs:2,advice:'safe',diagnostic:{appVersion:'1.0.6',upstreamStatus:null,responseType:null,failureStage:'connect',proxySource:'windows_static_configured',details:{origin,terminalEvent:null,upstreamFailure:null}}});
  inbox.ingestLogs([make('a'),make('b','stream_interrupted')]);expect(items).toHaveLength(1);expect(items[0].requests.map((r:any)=>r.id)).toEqual(['a','b']);
  inbox.markAllRead();inbox.ingestLogs([make('b'),make('c')]);expect(items[0].count).toBe(3);expect(items[0].read).toBe(true);
  inbox.ingestLogs([make('d','reauthorization_required','account')]);expect(items).toHaveLength(2);
  inbox.ingestLogs([{...make('next'),at:at+301}]);expect(items).toHaveLength(3);
  inbox.ingestLogs([make('recovered','success')]);expect(items).toHaveLength(3);
});

test('history and routine discovery or cancellation stay out of notifications', async () => {
  const result = (() => {
    let items: any[] = []; const store = createNotificationStore(v => { items = v; });
    const at = Math.ceil(Date.now() / 1000);
    const log = {id:'old',at:1,status:'network_error',conversationKey:'thread:same',account:null,operation:'responses',elapsedMs:1,advice:'safe'};
    store.ingestLogs([log]);
    const history = items.length;
    store.ingestLogs([log, {...log,id:'old-late',at:2}, {...log,id:'discovery',at,operation:'models'}, {...log,id:'cancel',at,status:'cancelled'}, {...log,id:'stop',at,status:'service_stopped'}]);
    const routine = items.length;
    store.ingestLogs([{...log,id:'new',at}]);
    return {history,routine,unread:items.filter(i=>!i.read).length,count:items.length};
  })();
  expect(result).toEqual({history:0,routine:0,unread:1,count:1});
});

test('notification grouping preserves unknown conversations, warning severity, and condition recurrence', async () => {
  const result = (() => {
    let items: any[] = [];
    const inbox = createNotificationStore((next: any[]) => { items = next; });
    inbox.ingestLogs([]);
    const start = Math.ceil(Date.now() / 1000);
    const log = (id: string, at: number, conversationKey?: string, status = 'unexpected_content_type') => ({ id, at: start + at, conversationKey, status, account: 'same-account', operation: 'responses', elapsedMs: 10, advice: 'safe diagnostic' });
    inbox.ingestLogs([log('unknown-a', 1000), log('unknown-b', 1001), log('a', 1002, 'thread:a'), log('b', 1302, 'thread:a'), log('limited', 1004, 'thread:a', 'upstream_rate_limited'), log('success', 1005, 'thread:a', 'success')]);
    const initial = items.map(n => ({ count: n.count, level: n.level }));
    inbox.markAllRead();
    inbox.ingestLogs([log('b', 1302, 'thread:a'), log('c', 1303, 'thread:a')]);
    const repeated = { count: items.find(n => n.requestId === 'c')?.count, unread: items.filter(n => !n.read).length };
    inbox.ingestLogs([log('d', 1904, 'thread:a')]);
    const later = { length: items.length, unread: items.filter(n => !n.read).length };
    const condition = { key: 'disk', notice: { title: '保存失败', message: 'safe error', level: 'warning' as const } };
    inbox.sync('status', [condition]); inbox.sync('status', [condition]);
    const stable = items.length;
    inbox.sync('status', []); inbox.sync('status', [condition]);
    const recurred = items.length;
    for (let i = 0; i < 120; i++) inbox.push({ title: '结果', message: String(i), level: 'info' });
    return { initial, repeated, later, stable, recurred, bounded: items.length };
  })();
  expect(result.initial).toHaveLength(4);
  expect(result.initial.filter((n: any) => n.count === 2)).toHaveLength(1);
  expect(result.initial.filter((n: any) => n.level === 'warning')).toHaveLength(1);
  expect(result.repeated).toEqual({ count: 3, unread: 0 });
  expect(result.later).toEqual({ length: 5, unread: 1 });
  expect(result.recurred).toBe(result.stable + 1);
  expect(result.bounded).toBe(100);
});
