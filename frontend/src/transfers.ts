import {cryptoClient} from './crypto';
import {HttpError} from './http-drive';
import type {Drive,PrivateChunk} from './types';
type Progress=(stage:string,percent:number,saved?:number)=>void;
export async function upload(drive:Drive,file:File,parent:string|null,key:string,signal:AbortSignal,progress:Progress){
  let versionId:string|undefined;
  try {
    const manifest=await cryptoClient.scan(file,value=>progress('Encrypting',value*35),signal);
    signal.throwIfAborted();const metadata=await cryptoClient.seal(manifest,key);
    progress('Checking blocks',35);
    const requirement=await drive.ingest({name:file.name,parent_id:parent,size:file.size,client_metadata:metadata,chunks:manifest.chunks.map(({object_id,size})=>({object_id,size}))},signal);
    versionId=requirement.version_id;
    const byId=new Map(manifest.chunks.map(chunk=>[chunk.object_id,chunk]));
    let sent=0;const total=requirement.upload_bytes;
    const physicalNew=requirement.missing_chunks.reduce((n,c)=>n+c.size,0);const saved=file.size-physicalNew;
    async function send(chunks:PrivateChunk[]){
      let next=0;
      await Promise.all(Array.from({length:Math.min(4,chunks.length)},async()=>{
        while(next<chunks.length){signal.throwIfAborted();const chunk=chunks[next++];const payload=await cryptoClient.encrypt(file,chunk.offset,chunk.size,signal);
          await drive.put(versionId!,chunk.object_id,payload,signal);sent+=payload.byteLength;progress('Uploading',35+Math.min(1,sent/Math.max(total,1))*55,saved);}
      }));
    }
    await send(requirement.missing_chunks.map(chunk=>byId.get(chunk.object_id)!));
    progress('Finalizing',94,saved);
    try {await drive.complete(versionId,signal);}catch(error){
      if(!(error instanceof HttpError)||error.status!==409||!error.details?.missing_object_ids)throw error;
      const missing=error.details.missing_object_ids as string[];
      if(missing.some(id=>!byId.has(id)))throw new Error('Server requested a block outside the file manifest');
      await send(missing.map(id=>byId.get(id)!));await drive.complete(versionId,signal);
    }
    progress('Uploaded',100,saved);return requirement;
  }catch(error){if(versionId)await drive.cancel(versionId).catch(()=>{});throw error;}
}
export async function download(drive:Drive,fileId:string,versionId:string,name:string,key:string,signal:AbortSignal,progress:Progress,handle?:FileSystemFileHandle){
  const version=await drive.version(fileId,versionId);
  if(version.status!=='ready')throw new Error('This version has not finished uploading');
  const manifest=await cryptoClient.open(version.client_metadata,key);
  if(manifest.version!==1||manifest.size!==version.size||manifest.chunks.length!==version.chunks.length||manifest.chunks.some((chunk,i)=>chunk.object_id!==version.chunks[i].object_id||chunk.size!==version.chunks[i].size))throw new Error('File manifest does not match the server references');
  let writable:FileSystemWritableFileStream|undefined;
  if(handle)writable=await handle.createWritable();
  if(!writable&&version.size>256*1024*1024)throw new Error('Use a browser with streaming file downloads for files larger than 256 MiB.');
  const parts:ArrayBuffer[]=[];const job=crypto.randomUUID();let done=0;
  await cryptoClient.call('download_begin',{job});
  try {
    for(const chunk of manifest.chunks){signal.throwIfAborted();const payload=await drive.block(chunk.object_id,signal);
      const plain=await cryptoClient.call<ArrayBuffer>('decrypt',{job,payload,fingerprint:chunk.fingerprint,objectId:chunk.object_id},undefined,signal);
      if(plain.byteLength!==chunk.size)throw new Error('Decrypted chunk size mismatch');
      if(writable)await writable.write(plain);else parts.push(plain);
      done+=plain.byteLength;progress('Downloading',done/Math.max(version.size,1)*95);
    }
    if(done!==version.size)throw new Error('File length verification failed');
    await cryptoClient.call('download_finish',{job,expected:manifest.sha256});
    if(writable)await writable.close();else saveBlob(new Blob(parts,{type:'application/octet-stream'}),name);
    progress('Downloaded',100);
  }catch(error){await writable?.abort().catch(()=>{});await cryptoClient.call('download_abort',{job});throw error;}
}
export function saveBlob(blob:Blob,name:string){const url=URL.createObjectURL(blob);const anchor=document.createElement('a');anchor.href=url;anchor.download=name;anchor.click();setTimeout(()=>URL.revokeObjectURL(url),60000);}
