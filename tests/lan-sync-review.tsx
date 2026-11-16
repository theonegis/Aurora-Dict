import React from 'react';
import {createRoot} from 'react-dom/client';
import {mockIPC} from '@tauri-apps/api/mocks';
import {LanSyncSettings} from '../src/LanSyncSettings';
import '../src/style.css';
Object.defineProperty(window,'isTauri',{value:true,configurable:true});
let enabled=false;
const note={uid:'a'.repeat(32),word:'kindness',phonetic:'/ˈkaɪndnəs/',definitionMarkdown:'友善；体贴',examplesMarkdown:'Thank you for your kindness.',deleted:false,version:'b'.repeat(32)};
let conflicts=[{id:'test',remote:note,candidate:{...note,definitionMarkdown:'善意；仁慈',examplesMarkdown:'A small act of kindness matters.'}}];
const calls:string[]=[];
Object.assign(window,{syncReviewCalls:calls});
mockIPC((command,args)=>{
 calls.push(command);
 if(command==='lan_sync_status')return {enabled,name:'测试电脑',host:'aurora-test.local.',port:12345,peers:enabled?[{id:'phone',name:'测试 iPhone'}]:[]};
 if(command==='lan_sync_conflicts')return enabled?conflicts:[];
 if(command==='lan_sync_enable'){enabled=Boolean(args && !Array.isArray(args) && !(args instanceof ArrayBuffer) && 'enabled' in args && args.enabled);return null;}
 if(command==='lan_sync_pairing')return {pairing:{protocol:1,host:'fixture.local'},svg:'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 240"><rect width="240" height="240" fill="white"/><text x="30" y="120">UI test QR placeholder</text></svg>'};
 if(command==='lan_sync_resolve'){conflicts=[];return null;}
 return null;
});
createRoot(document.getElementById('root')!).render(<main style={{maxWidth:650,margin:'20px auto',padding:16}}><LanSyncSettings/></main>);
