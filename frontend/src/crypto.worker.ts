/// <reference lib="webworker" />
import type {PrivateChunk,PrivateManifest} from './types';
import init,* as module from './wasm/ruxredock_wasm.js';
type Hasher={update:(data:Uint8Array)=>void;finish:()=>string;free:()=>void};
type Wasm={default:()=>Promise<unknown>;hash:(data:Uint8Array)=>string;encrypt:(data:Uint8Array)=>Uint8Array;decrypt:(data:Uint8Array,fingerprint:string,objectId:string)=>Uint8Array;Chunker:new(min:number,avg:number,max:number,window:number)=>{feed:(data:Uint8Array)=>Uint32Array;free:()=>void};FileHasher:new()=>Hasher};
const ready=init().then(()=>module as Wasm);
const cancelled=new Set<number>();
const downloadHashes=new Map<string,Hasher>();
const aad=new TextEncoder().encode('ruxredock-private-manifest-v1');
function concat(a:Uint8Array,b:Uint8Array){const result=new Uint8Array(a.length+b.length);result.set(a);result.set(b,a.length);return result;}
function toBase64(data:Uint8Array){let text='';for(let i=0;i<data.length;i+=32768)text+=String.fromCharCode(...data.subarray(i,i+32768));return btoa(text);}
function fromBase64(data:string){return Uint8Array.from(atob(data),c=>c.charCodeAt(0));}
async function keyFromHex(value:string){if(!/^[a-f0-9]{64}$/i.test(value))throw new Error('Recovery key must contain 64 hexadecimal characters');return crypto.subtle.importKey('raw',Uint8Array.from(value.match(/../g)!,n=>parseInt(n,16)),'AES-GCM',false,['encrypt','decrypt']);}
async function scan(file:File,id:number):Promise<PrivateManifest>{
  const wasm=await ready;
  const config={min_size:16384,avg_size:65536,max_size:262144,window_size:64};
  const chunker=new wasm.Chunker(config.min_size,config.avg_size,config.max_size,config.window_size);
  const hasher=new wasm.FileHasher();const chunks:PrivateChunk[]=[];
  let tail:Uint8Array=new Uint8Array(0);let start=0;
  function record(data:Uint8Array){const payload=wasm.encrypt(data);chunks.push({offset:start,size:data.length,fingerprint:wasm.hash(data),object_id:wasm.hash(payload)});start+=data.length;if(chunks.length>250000)throw new Error('This file exceeds the 250,000-block upload limit');}
  try {
    for(let offset=0;offset<file.size;offset+=1048576){
      if(cancelled.has(id))throw new DOMException('Cancelled','AbortError');
      const data=new Uint8Array(await file.slice(offset,offset+1048576).arrayBuffer());hasher.update(data);
      const cuts=chunker.feed(data);let previous=0;
      for(const cut of cuts){record(concat(tail,data.subarray(previous,cut)));tail=new Uint8Array(0);previous=cut;}
      tail=concat(tail,data.subarray(previous));
      self.postMessage({id,progress:Math.min(1,(offset+data.length)/Math.max(file.size,1))});
    }
    if(tail.length)record(tail);
    return {version:1,name:file.name,size:file.size,sha256:hasher.finish(),chunks,chunking:config};
  } finally {chunker.free();hasher.free();cancelled.delete(id);}
}
self.onmessage=async({data})=>{
  if(data.operation==='cancel'){cancelled.add(data.target);return;}
  const {id,operation}=data;
  try {
    const wasm=await ready;let value:unknown;
    switch(operation){
      case 'scan':value=await scan(data.file,id);break;
      case 'encrypt':value=new Uint8Array(wasm.encrypt(new Uint8Array(await data.file.slice(data.offset,data.offset+data.size).arrayBuffer()))).buffer;break;
      case 'seal': {
        const nonce=crypto.getRandomValues(new Uint8Array(12));const key=await keyFromHex(data.key);
        const encrypted=new Uint8Array(await crypto.subtle.encrypt({name:'AES-GCM',iv:nonce,additionalData:aad},key,new TextEncoder().encode(JSON.stringify(data.manifest))));
        value=toBase64(concat(nonce,encrypted));break;
      }
      case 'open': {
        const payload=fromBase64(data.metadata);const key=await keyFromHex(data.key);
        const plain=await crypto.subtle.decrypt({name:'AES-GCM',iv:payload.slice(0,12),additionalData:aad},key,payload.slice(12));
        value=JSON.parse(new TextDecoder().decode(plain));break;
      }
      case 'download_begin':downloadHashes.set(data.job,new wasm.FileHasher());value=true;break;
      case 'decrypt': {
        const plain=wasm.decrypt(new Uint8Array(data.payload),data.fingerprint,data.objectId);
        downloadHashes.get(data.job)?.update(plain);value=new Uint8Array(plain).buffer;break;
      }
      case 'download_finish': {
        const hasher=downloadHashes.get(data.job);if(!hasher)throw new Error('Missing download verification state');
        try{if(hasher.finish()!==data.expected)throw new Error('File integrity check failed');value=true;}finally{hasher.free();downloadHashes.delete(data.job);}break;
      }
      case 'download_abort':downloadHashes.get(data.job)?.free();downloadHashes.delete(data.job);value=true;break;
      default:throw new Error('Unknown encryption operation');
    }
    if(value instanceof ArrayBuffer)self.postMessage({id,value},{transfer:[value]});else self.postMessage({id,value});
  }catch(error){self.postMessage({id,error:operation==='open'?'This recovery key cannot unlock the file. Use the key from its original workspace.':error instanceof Error?error.message:String(error)});}
};
