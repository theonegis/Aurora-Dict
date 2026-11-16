import {useEffect,useState} from 'react';
import {invoke,isTauri} from '@tauri-apps/api/core';
import {writeText} from '@tauri-apps/plugin-clipboard-manager';
type Note={uid:string;word:string;phonetic:string;definitionMarkdown:string;examplesMarkdown:string;deleted:boolean;version:string};
type Conflict={id:string;candidate:Note;remote:Note};
type Status={enabled:boolean;name?:string;host?:string;port?:number;discoveryError?:string;peers:{id:string;name:string}[]};
export function LanSyncSettings({english=false}:{english?:boolean}){
 const tr=(zh:string,en:string)=>english?en:zh;
 const [status,setStatus]=useState<Status>({enabled:false,peers:[]});
 const [pair,setPair]=useState<{pairing:Record<string,unknown>;svg:string}|null>(null);
 const [conflicts,setConflicts]=useState<Conflict[]>([]),[error,setError]=useState(''),[busy,setBusy]=useState(false),[host,setHost]=useState('');
 const refresh=async()=>{if(!isTauri())return;const [s,c]=await Promise.all([invoke<Status>('lan_sync_status'),invoke<Conflict[]>('lan_sync_conflicts')]);setStatus(s);setConflicts(c);};
 useEffect(()=>{let live=true;const run=()=>refresh().catch(e=>{if(live)setError(String(e));});void run();const timer=setInterval(()=>void run(),4000);return()=>{live=false;clearInterval(timer);};},[]);
 const action=async(fn:()=>Promise<unknown>)=>{setBusy(true);setError('');try{await fn();await refresh();}catch(e){setError(String(e));}finally{setBusy(false);}};
 return <section className="settings-module settings-card lan-sync-card" aria-label={tr('设备与同步','Devices and sync')}>
  <div className="lan-sync-heading"><h3><i className="fa-solid fa-arrows-rotate" aria-hidden="true" />{tr('设备与同步','Devices and sync')}</h3><label><input type="checkbox" checked={status.enabled} disabled={busy||!isTauri()} onChange={e=>{const enabled=e.target.checked;void action(async()=>{await invoke('lan_sync_enable',{enabled});setPair(null);});}}/>{tr('局域网同步','LAN sync')}</label></div>
  <p className="lan-sync-hint">{status.enabled?tr('手机首次配对后，打开 App 自动同步生词本。','After pairing, opening the mobile app syncs your vocabulary.'):tr('开启后，可与同一局域网内的 iPhone 和 Android 手机配对。','Enable to pair with iOS and Android on your local network.')}</p>
  {error&&<p role="alert">{error}</p>}
  {status.discoveryError&&<p role="status">{tr('自动发现暂不可用，可填写本机局域网 IP 后生成配对码。','Discovery is unavailable. Enter this computer’s LAN IP to pair.')}</p>}
  {status.enabled&&<>
   <div className="lan-sync-actions"><button className="quiet-button" disabled={busy} onClick={()=>void action(async()=>setPair(await invoke('lan_sync_pairing',{host:host||null})))}>{tr('配对手机 / 刷新二维码','Pair phone / refresh QR')}</button><span className="lan-sync-hint">{status.name}</span></div>
   <details><summary>{tr('手动地址（自动发现失败时）','Manual address')}</summary><input aria-label={tr('电脑局域网地址','Computer LAN address')} value={host} onChange={e=>setHost(e.target.value)} placeholder="192.168.1.20"/><p className="lan-sync-hint">{status.host}:{status.port}</p></details>
   {pair&&<div className="lan-sync-pair"><img width={240} height={240} src={`data:image/svg+xml,${encodeURIComponent(pair.svg)}`} alt={tr('手机配对二维码','Phone pairing QR code')}/><p className="lan-sync-hint">{tr('用手机 App 的「设备与同步」扫码。配对码5分钟有效，仅用于一台设备。','Scan in the mobile app. Valid for 5 minutes and one device.')}</p><button className="quiet-button" onClick={()=>void action(()=>writeText(JSON.stringify(pair.pairing)))}>{tr('复制配对信息','Copy pairing data')}</button></div>}
   <ul className="lan-sync-peers">{status.peers.map(peer=><li key={peer.id}><span>{peer.name}</span><button className="quiet-button" disabled={busy} onClick={()=>void action(()=>invoke('lan_sync_revoke',{device:peer.id}))}>{tr('取消配对','Unpair')}</button></li>)}</ul>
  </>}
  {conflicts.map(c=><article className="lan-sync-conflict" key={c.id}><h4>{c.remote.word} · {tr('修改冲突','Conflict')}</h4><div className="lan-sync-versions">{([[c.candidate,tr('手机修改','Phone changes'),true],[c.remote,tr('电脑内容','Computer version'),false]] as const).map(([note,label,candidate])=><div key={label}><b>{label}</b><p>{note.deleted?tr('删除此单词','Delete this word'):note.word}</p>{!note.deleted&&<pre>{[note.phonetic,note.definitionMarkdown,note.examplesMarkdown].filter(Boolean).join('\n\n')}</pre>}<button className="quiet-button" disabled={busy} onClick={()=>void action(()=>invoke('lan_sync_resolve',{id:c.id,useCandidate:candidate,expectedVersion:c.remote.version}))}>{tr('使用此版本','Use this version')}</button></div>)}</div></article>)}
 </section>;
}
