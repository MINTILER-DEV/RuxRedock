import {cryptoClient,randomKey} from './crypto';
import type {Account,Drive,Entry,Folder,Requirements,UploadInput,Version,VersionManifest} from './types';
type State={folders:Folder[];files:Entry[];versions:Record<string,VersionManifest&{created_at:string}>};
function transaction<T>(db:IDBDatabase,store:string,mode:IDBTransactionMode,operation:(store:IDBObjectStore)=>IDBRequest<T>):Promise<T>{return new Promise((resolve,reject)=>{const tx=db.transaction(store,mode);const request=operation(tx.objectStore(store));tx.oncomplete=()=>resolve(request.result);tx.onerror=()=>reject(tx.error);tx.onabort=()=>reject(tx.error||new Error('Local storage transaction was cancelled'));});}
export class DemoDrive implements Drive {
  readonly demo=true;
  private constructor(private db:IDBDatabase,private state:State){}
  static async open():Promise<DemoDrive>{
    const db=await new Promise<IDBDatabase>((resolve,reject)=>{const request=indexedDB.open('ruxredock-demo-v1',1);request.onupgradeneeded=()=>{request.result.createObjectStore('metadata');request.result.createObjectStore('blocks');};request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
    const saved=await transaction<State|undefined>(db,'metadata','readonly',store=>store.get('drive'));
    const drive=new DemoDrive(db,saved||{folders:[],files:[],versions:{}});
    if(!saved)await drive.seed();return drive;
  }
  private persist(){return transaction(this.db,'metadata','readwrite',store=>store.put(this.state,'drive'));}
  async me():Promise<Account>{return {id:'local-demo',name:'Alex Morgan',used_bytes:Object.values(this.state.versions).reduce((sum,v)=>sum+v.size,0),quota_bytes:107374182400};}
  async list(parent:string|null){return {files:this.state.files.filter(f=>f.parent_id===parent),directories:this.state.folders.filter(f=>f.parent_id===parent)};}
  async folder(name:string,parent:string|null){this.validateName(name);if(parent&&!this.state.folders.some(f=>f.id===parent))throw new Error('Parent folder not found');if([...this.state.folders,...this.state.files].some(f=>f.parent_id===parent&&f.name===name))throw new Error('This name is already in use');const folder={id:crypto.randomUUID(),name,parent_id:parent};this.state.folders.push(folder);await this.persist();return folder;}
  async versions(fileId:string):Promise<Version[]>{return Object.values(this.state.versions).filter(v=>v.file_id===fileId).sort((a,b)=>b.created_at.localeCompare(a.created_at)).map(v=>({id:v.version_id,size:v.size,status:v.status,created_at:v.created_at}));}
  async version(fileId:string,versionId:string){const version=this.state.versions[versionId];if(!version||version.file_id!==fileId)throw new Error('Version not found');return version;}
  async ingest(input:UploadInput):Promise<Requirements>{
    this.validateName(input.name);if(input.parent_id&&!this.state.folders.some(f=>f.id===input.parent_id))throw new Error('Parent folder not found');if(this.state.folders.some(f=>f.parent_id===input.parent_id&&f.name===input.name))throw new Error('A folder already uses this name');let file=this.state.files.find(f=>f.name===input.name&&f.parent_id===input.parent_id);
    if(!file){file={id:crypto.randomUUID(),name:input.name,parent_id:input.parent_id,size:input.size,modified:new Date().toISOString(),version_count:0,status:'pending'};this.state.files.push(file);}
    const id=crypto.randomUUID();this.state.versions[id]={file_id:file.id,version_id:id,size:input.size,status:'pending',client_metadata:input.client_metadata,chunks:input.chunks,created_at:new Date().toISOString()};
    file.version_count++;await this.persist();
    const found=new Set<string>();for(const chunk of input.chunks)if(await transaction(this.db,'blocks','readonly',store=>store.getKey(chunk.object_id)))found.add(chunk.object_id);
    const missing=input.chunks.map(c=>!found.has(c.object_id));const missingChunks=[...new Map(input.chunks.filter(c=>!found.has(c.object_id)).map(c=>[c.object_id,c])).values()];
    return {file_id:file.id,version_id:id,status:'pending',missing,missing_chunks:missingChunks,upload_bytes:missingChunks.reduce((n,c)=>n+c.size+23,0)};
  }
  async put(versionId:string,objectId:string,payload:ArrayBuffer){const version=this.state.versions[versionId];if(!version?.chunks.some(c=>c.object_id===objectId))throw new Error('Block does not belong to upload');await transaction(this.db,'blocks','readwrite',store=>store.put(payload,objectId));}
  async complete(versionId:string){const version=this.state.versions[versionId];if(!version)throw new Error('Upload not found');for(const chunk of version.chunks)if(!await transaction(this.db,'blocks','readonly',store=>store.getKey(chunk.object_id)))throw new Error('Upload is incomplete');version.status='ready';const file=this.state.files.find(f=>f.id===version.file_id)!;file.status='ready';file.size=version.size;file.modified=version.created_at;await this.persist();}
  async cancel(versionId:string){const version=this.state.versions[versionId];if(!version)return;if(version.status==='ready')throw new Error('Upload has already completed');delete this.state.versions[versionId];const file=this.state.files.find(f=>f.id===version.file_id);if(file){file.version_count--;if(!file.version_count)this.state.files=this.state.files.filter(f=>f.id!==file.id);}await this.persist();}
  async block(objectId:string){const payload=await transaction<ArrayBuffer|undefined>(this.db,'blocks','readonly',store=>store.get(objectId));if(!payload)throw new Error('Encrypted block is missing');return payload;}
  private validateName(name:string){if(!name||Array.from(name).length>255||/[\p{Cc}/\\]/u.test(name)||name==='.'||name==='..')throw new Error('Choose a valid name without path separators');}
  async rename(id:string,name:string,folder:boolean){this.validateName(name);const entries=folder?this.state.folders:this.state.files;const item=entries.find(f=>f.id===id);if(!item)throw new Error('Item not found');if([...this.state.folders,...this.state.files].some(f=>f.id!==id&&f.parent_id===item.parent_id&&f.name===name))throw new Error('This name is already in use');item.name=name;await this.persist();}
  async remove(id:string,folder:boolean){if(folder){if(this.state.folders.some(f=>f.parent_id===id)||this.state.files.some(f=>f.parent_id===id))throw new Error('Empty the folder before deleting it');this.state.folders=this.state.folders.filter(f=>f.id!==id);}else{this.state.files=this.state.files.filter(f=>f.id!==id);for(const version of Object.values(this.state.versions))if(version.file_id===id)delete this.state.versions[version.version_id];}await this.persist();}
  private async seed(){
    const projects=await this.folder('Projects',null);await this.folder('Website redesign',projects.id);
    const documents=await this.folder('Documents',null);await this.folder('Finance',null);await this.folder('Design assets',null);
    const text=new TextEncoder();
    const svg='<svg xmlns="http://www.w3.org/2000/svg" width="960" height="640"><rect width="960" height="640" fill="#eeeafa"/><rect x="80" y="80" width="800" height="480" rx="24" fill="white"/><text x="135" y="175" font-family="sans-serif" font-size="36" fill="#6553dc">A little space for big ideas.</text><rect x="135" y="220" width="690" height="2" fill="#ddd"/><text x="135" y="300" font-family="sans-serif" font-size="22">RuxRedock / Interface exploration</text></svg>';
    const samples=[
      ['Project brief.md','# Website redesign\n\nA calm, clear workspace for the next chapter.\n\n## Goals\n- Make information easy to find\n- Keep every version within reach\n- Build with accessibility in mind\n',null],
      ['Interface exploration.svg',svg,null],
      ['Q4 budget.csv','Category,Planned,Actual\nDesign,12000,10400\nEngineering,32000,31500\nResearch,6000,4800\nOperations,4500,4200\n',null],
      ['Meeting notes.md','# Weekly sync\n\n## Decisions\n- Review the first prototype on Friday\n- Keep the file list simple\n- Share feedback in the project brief\n\n## Next steps\nPrepare the next round of designs.\n',null],
      ['Workspace.json','{\n  "project": "RuxRedock",\n  "milestone": "Browser workspace",\n  "status": "in progress"\n}\n',null],
      ['Getting started.txt','Welcome to your demo workspace.\n\nOpen folders, upload a file, download it again, or browse its versions.\nDemo files stay in this browser. Connect your server to use your private account.\n',null],
      ['Architecture.md','# Architecture\n\nClient-side chunking and convergent encryption.\nPostgreSQL metadata. Redis block index. S3-compatible ciphertext storage.\n',documents.id],
      ['Roadmap.md','# Project roadmap\n\n1. Local proof of concept\n2. Metadata API\n3. Browser file manager and storage\n',projects.id],
    ] as const;
    for(let i=0;i<samples.length;i++){
      const [name,content,parent]=samples[i];const file=new File([text.encode(content)],name);
      const manifest=await cryptoClient.scan(file);const metadata=await cryptoClient.seal(manifest,demoKey());
      const result=await this.ingest({name,parent_id:parent,size:file.size,client_metadata:metadata,chunks:manifest.chunks.map(({object_id,size})=>({object_id,size}))});
      for(const chunk of manifest.chunks)await this.put(result.version_id,chunk.object_id,await cryptoClient.encrypt(file,chunk.offset,chunk.size));
      await this.complete(result.version_id);
      const modified=new Date(Date.now()-(i+1)*86400000).toISOString();this.state.versions[result.version_id].created_at=modified;this.state.files.find(f=>f.id===result.file_id)!.modified=modified;
    }
    await this.persist();
  }
}
export function demoKey(){let key=localStorage.getItem('ruxredock-demo-key');if(!key){key=randomKey();localStorage.setItem('ruxredock-demo-key',key);}return key;}
