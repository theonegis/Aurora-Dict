import React, { act, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { VocabularyPanel } from '../src/VocabularyPanel';
import { VocabularyQuiz } from '../src/VocabularyQuiz';
import { PronunciationRow } from '../src/Pronunciation';
import { playPronunciation } from '../src/audio';
import { cacheSourceResults, readCachedQuery } from '../src/storage';
import { copy } from '../src/config';
import type { VocabularyEntry, VocabularyQuizQuestion } from '../src/types';

// Isolated browser harness: no Tauri commands or user vocabulary files are used.
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const fixture = document.querySelector<HTMLDivElement>('#fixture')!;
const results = document.querySelector('#results')!;
const assert = (value: unknown, message: string) => { if (!value) throw new Error(message); };
const deferred = <T,>() => { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const entry: VocabularyEntry = { id: 1, word: 'kindness', phonetic: '', definitionMarkdown: '善意', examplesMarkdown: '', createdAt: 1, updatedAt: 1 };
const button = (label: string) => [...fixture.querySelectorAll('button')].find((element) => element.textContent?.trim() === label)!;
const t = (key: keyof typeof copy.zh) => copy.zh[key];

const cases: [string, () => Promise<void>][] = [
 ['pronunciation remains available without IPA', async () => {
   const root = createRoot(fixture);
   try { await act(() => root.render(<PronunciationRow word="kindness" ukAudio="https://example.com/audio" t={t} />)); assert(fixture.querySelectorAll('button').length === 2, 'missing pronunciation buttons'); }
   finally { await act(() => root.unmount()); }
 }],
 ['saving locks the editor and prevents duplicate writes', async () => {
   const root = createRoot(fixture); const pending = deferred<VocabularyEntry>(); let writes = 0;
   const status = {path:'test.sqlite3',entryCount:1};
   try {
     function Host() { const [entries, setEntries] = useState([entry]); return <VocabularyPanel entries={entries} status={status} loading={false} error="" isDesktop={false} persist={async () => { writes++; const saved = await pending.promise; setEntries([saved]); return saved; }} remove={async () => {}} refresh={async () => {}} importBook={async () => status} exportBook={async () => status} moveBook={async () => status} openBook={async () => status} t={t} />; }
     await act(() => root.render(<Host/>));
     await act(() => button('编辑').click());
     const input = fixture.querySelector<HTMLInputElement>('.vocabulary-word-fields input')!;
     await act(() => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value')!.set!.call(input,'kindness-edited'); input.dispatchEvent(new Event('input',{bubbles:true})); });
     await act(() => button('保存生词').click());
     assert(fixture.querySelector('fieldset')?.disabled, 'editor not locked');
     assert(input.matches(':disabled'), 'word input is still editable');
     await act(() => document.dispatchEvent(new KeyboardEvent('keydown',{key:'s',ctrlKey:true,bubbles:true})));
     assert(writes===1,'duplicate save');
     await act(async () => { pending.resolve({...entry,word:'kindness-edited'}); await pending.promise; });
     assert(!fixture.querySelector('fieldset')?.disabled,'editor remained locked');
     assert(fixture.textContent?.includes('kindness-edited'),'saved entry not shown');
   } finally { await act(() => root.unmount()); }
 }],
 ['quiz restarts an in-flight question when its generator changes', async () => {
   const root=createRoot(fixture); const old=deferred<VocabularyQuizQuestion>(); const fresh=deferred<VocabularyQuizQuestion>();
   const props={entries:[entry],modelReady:true,modelName:'test',openSettings:()=>{},t}; let calls=0;
   try {
     await act(()=>root.render(<VocabularyQuiz {...props} generateQuestion={()=>old.promise}/>));
     await act(()=>button(t('quizStart')).click());
     await act(()=>root.render(<VocabularyQuiz {...props} generateQuestion={()=>{calls++;return fresh.promise;}}/>));
     assert(calls===1,'new generator did not start');
     const question: VocabularyQuizQuestion={entryId:1,word:'kindness',sentence:'Her ____ helped.',translation:'她的善意很有帮助。',options:['kindness','table','apple','house'],correctIndex:0,explanation:'需要善意。'};
     await act(async()=>{fresh.resolve(question);await fresh.promise;});
     await act(async()=>{old.resolve({...question,sentence:'STALE'});await old.promise;});
     assert(fixture.textContent?.includes('Her ____ helped.'),'new question missing');
     assert(!fixture.textContent?.includes('STALE'),'stale question overwrote newer question');
     assert(!fixture.textContent?.includes(t('quizGenerating')),'loading stuck');
   } finally {await act(()=>root.unmount());}
 }],
 ['pronunciation rejects stale fallback and stops previous recording',async()=>{
   const OriginalAudio=window.Audio; const originalVoice=window.SpeechSynthesisUtterance; const synth=Object.getOwnPropertyDescriptor(window,'speechSynthesis');
   const sounds:{pause:()=>void, play:()=>Promise<void>}[]=[]; const first=deferred<void>(); let paused=0;let spoken=0;
   Object.defineProperty(window,'speechSynthesis',{configurable:true,value:{cancel:()=>{},speak:()=>{spoken++;},getVoices:()=>[]}});
   window.SpeechSynthesisUtterance=class{} as typeof SpeechSynthesisUtterance;
   window.Audio=class {constructor(){sounds.push(this);} pause(){paused++;} play(){return sounds.length===1 ? first.promise:Promise.resolve();}} as unknown as typeof Audio;
   try {const a=playPronunciation('old','en-US','https://example.com/a'); await playPronunciation('new','en-US','https://example.com/b');first.reject(new Error('cancelled'));await a;assert(paused===1,'old audio was not paused');assert(spoken===0,'stale speech fallback started');}
   finally {window.Audio=OriginalAudio;window.SpeechSynthesisUtterance=originalVoice;if(synth)Object.defineProperty(window,'speechSynthesis',synth);else delete (window as unknown as Record<string,unknown>).speechSynthesis;}
 }],
 ['case-sensitive lookup caches stay separate',async()=>{
   const model=(content:string)=>({type:'llm' as const,result:{word:content,content,modelId:'qwen3-0.6b' as const,modelName:'test',note:''}});
   await cacheSourceResults('REVIEW-US',{local_llm:model('country')},50);
   await cacheSourceResults('review-us',{local_llm:model('pronoun')},50);
   const a=await readCachedQuery('REVIEW-US'); const b=await readCachedQuery('review-us');
   assert(a?.results.local_llm?.type==='llm' && a.results.local_llm.result.content==='country','upper-case result lost');
   assert(b?.results.local_llm?.type==='llm' && b.results.local_llm.result.content==='pronoun','lower-case result lost');
 }],
];
document.querySelector<HTMLButtonElement>('#run')!.onclick=async()=>{
 results.textContent='';
 for(const [name,run] of cases){try{await run();results.textContent+=`PASS ${name}\n`;}catch(error){results.textContent+=`FAIL ${name}: ${String(error)}\n`;}}
 results.textContent+='DONE';
};
