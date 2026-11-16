import React, { act } from 'react';
import { createRoot } from 'react-dom/client';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';
import App from '../src/App';
import { defaultSettings, SETTINGS_STORAGE_KEY } from '../src/config';
import type { VocabularyEntry } from '../src/types';
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const fixture = document.querySelector<HTMLDivElement>('#fixture')!;
const results = document.querySelector('#results')!;
const assert = (value: unknown, message: string) => { if (!value) throw new Error(message); };
const deferred = <T,>() => { let resolve!: (value:T)=>void; const promise=new Promise<T>(r=>resolve=r); return {promise,resolve}; };
const pause = () => new Promise(r=>setTimeout(r,40));
const click = async (selector:string,index=0) => { const el=fixture.querySelectorAll<HTMLButtonElement>(selector)[index]; assert(el,`missing ${selector}`); await act(()=>el.click()); };
const fill = async (selector:string,value:string,index=0) => { const el=fixture.querySelectorAll<HTMLInputElement|HTMLTextAreaElement>(selector)[index]; assert(el,`missing ${selector}`); await act(()=>{Object.getOwnPropertyDescriptor(el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value')!.set!.call(el,value);el.dispatchEvent(new Event('input',{bubbles:true}));}); };
const oldEntry:VocabularyEntry={id:1,word:'existing-note',phonetic:'',definitionMarkdown:'existing',examplesMarkdown:'',createdAt:1,updatedAt:1};
let override:(command:string)=>unknown;
function setup(){
 localStorage.setItem(SETTINGS_STORAGE_KEY,JSON.stringify({...defaultSettings,enabledSources:['local','local_llm']}));
 mockWindows('main');
 mockIPC((command)=>{
  const custom=override(command); if(custom!==undefined)return custom;
  if(command==='list_vocabulary_entries')return [oldEntry];
  if(command==='vocabulary_book_status')return {path:'test.sqlite3',entryCount:1};
  if(command==='llm_status')return {engineAvailable:false,models:[]};
  if(command==='lookup_local')return {query:'review-word',sampleData:false,suggestions:[],entries:[{word:'review-word',phonetic:'',pos:'n.',translation:'测试',definition:'test'}]};
  if(command==='suggest_local_words')return {suggestions:[],correction:false};
  if(command.includes('font'))return [];
  if(command==='lookup_word_graph')return {word:'review-word',nodes:[],edges:[],sources:[],warnings:[]};
  return false;
 },{shouldMockEvents:true});
}
const cases:[string,()=>Promise<void>][]=[
 ['source selected while cache loads is queried once',async()=>{
  let aiCalls=0;override=cmd=>cmd==='lookup_llm'?(aiCalls++,{word:'review-word',content:'FRESH_AI_RESULT',modelId:'qwen3-0.6b',modelName:'test',note:''}):undefined;
  setup();const root=createRoot(fixture);const open=IDBFactory.prototype.open;let release=()=>{};
  try{
   await act(()=>root.render(<App/>));
   IDBFactory.prototype.open=function(){const request={onerror:null as null|(()=>void),error:new Error('isolated cache unavailable')};release=()=>request.onerror?.();return request as unknown as IDBOpenDBRequest;};
   await fill('.search-box input','review-word');await click('.search-submit');await click('.source-tab',1);
   assert(aiCalls===0,'source started before cache hydration');
   IDBFactory.prototype.open=open;await act(async()=>{release();await pause();});
   assert(aiCalls===1,'AI request duplicated or lost');assert(fixture.textContent?.includes('FRESH_AI_RESULT'),'fresh result overwritten');
  }finally{IDBFactory.prototype.open=open;await act(()=>root.unmount());clearMocks();}
 }],
 ['changing lookup prompt does not cancel pending translation',async()=>{
  const translation=deferred<unknown>();override=cmd=>cmd==='translate_llm'?translation.promise:undefined;setup();const root=createRoot(fixture);
  try{
   await act(()=>root.render(<App/>));await click('.quick-action',1);await fill('.translation-form textarea','hello');await click('.translation-form button');
   await click('.quick-action',4);await click('.settings-category-tab',1);await fill('.prompt-editor textarea','A changed dictionary prompt',0);
   await act(async()=>{translation.resolve({source:'hello',translation:'TRANSLATION_COMPLETED',modelId:'qwen3-0.6b',modelName:'test',note:''});await pause();});
   await click('.quick-action',1);assert(fixture.textContent?.includes('TRANSLATION_COMPLETED'),'unrelated prompt cancelled translation');
  }finally{await act(()=>root.unmount());clearMocks();}
 }],
 ['adding during initial vocabulary load preserves existing notes',async()=>{
  const initial=deferred<VocabularyEntry[]>();let reads=0;const fresh={...oldEntry,id:2,word:'review-word'};
  override=cmd=>cmd==='list_vocabulary_entries'?(++reads===1?initial.promise:[fresh,oldEntry]):cmd==='add_vocabulary_entry'?fresh:undefined;
  setup();const root=createRoot(fixture);
  try{
   await act(()=>root.render(<App/>));await fill('.search-box input','review-word');await click('.search-submit');await act(async()=>{await pause();});
   const add=[...fixture.querySelectorAll<HTMLButtonElement>('button')].find(b=>b.textContent?.trim()==='加入生词本');assert(add,'missing add button');await act(async()=>{add!.click();await pause();});
   await act(async()=>{initial.resolve([oldEntry]);await pause();});await click('.quick-action',2);
   assert(reads>=2,'interrupted initial list was not refreshed');assert(fixture.textContent?.includes('existing-note'),'existing note disappeared');assert(fixture.textContent?.includes('review-word'),'new note disappeared');
  }finally{await act(()=>root.unmount());clearMocks();}
 }],
];
document.querySelector<HTMLButtonElement>('#run')!.onclick=async()=>{
 const saved=localStorage.getItem(SETTINGS_STORAGE_KEY);results.textContent='';
 try{for(const[name,run]of cases){try{await run();results.textContent+=`PASS ${name}\n`;}catch(error){results.textContent+=`FAIL ${name}: ${String(error)}\n`;}}}
 finally{if(saved===null)localStorage.removeItem(SETTINGS_STORAGE_KEY);else localStorage.setItem(SETTINGS_STORAGE_KEY,saved);}
 results.textContent+='DONE';
};
