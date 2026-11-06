import { test, expect } from '@playwright/test';
import { createReadCoordinator } from '../../src/refresh';
test('a pending module neither blocks refresh nor multiplies its in-flight reads', async () => {
  const result = await (async () => {
    let slowCalls=0, fastCalls=0;
    const errors: string[]=[];
    const reader=createReadCoordinator((name:string,error:string|null)=>{if(error)errors.push(name);},30);
    const tasks=[{name:'slow',read:()=>{slowCalls++;return new Promise(()=>{});},update:()=>{}},{name:'fast',read:async()=>++fastCalls,update:()=>{}}];
    reader.refresh(tasks);
    await new Promise(r=>setTimeout(r,60));
    reader.refresh(tasks);
    await new Promise(r=>setTimeout(r,10));
    reader.dispose();
    return {slowCalls,fastCalls,errors};
  })();
  expect(result).toEqual({slowCalls:1,fastCalls:2,errors:['slow']});
});
test('late reads cannot overwrite post-mutation data or update a disposed view', async () => {
  const values=await (async()=>{
    const out:string[]=[];let release:(v:string)=>void=()=>{};
    const reader=createReadCoordinator(()=>{});
    reader.refresh([{name:'data',read:()=>new Promise<string>(r=>{release=r;}),update:(v:string)=>out.push(v)}]);
    await new Promise(r=>setTimeout(r,0));
    reader.invalidate();
    reader.refresh([{name:'data',read:async()=>'fresh',update:(v:string)=>out.push(v)}]);
    release('old');await new Promise(r=>setTimeout(r,10));
    reader.refresh([{name:'data',read:()=>new Promise<string>(r=>{release=r;}),update:(v:string)=>out.push(v)}]);
    await new Promise(r=>setTimeout(r,0));reader.dispose();release('disposed');await new Promise(r=>setTimeout(r,0));
    return out;
  })();
  expect(values).toEqual(['fresh']);
});
