import type {PrivateManifest} from './types';
type Pending={resolve:(value:any)=>void;reject:(error:Error)=>void;progress?:(value:number)=>void;cleanup:()=>void};
export class CryptoClient {
  private worker=new Worker(new URL('./crypto.worker.ts',import.meta.url),{type:'module'});
  private pending=new Map<number,Pending>();
  private next=0;
  constructor(){
    this.worker.onmessage=({data})=>{
      const item=this.pending.get(data.id);if(!item)return;
      if(data.progress!==undefined){item.progress?.(data.progress);return;}
      this.pending.delete(data.id);item.cleanup();
      if(data.error)item.reject(new Error(data.error));else item.resolve(data.value);
    };
    this.worker.onerror=(event)=>{for(const item of this.pending.values()){item.cleanup();item.reject(new Error(event.message||'Encryption worker could not start'));}this.pending.clear();};
  }
  call<T>(operation:string,args:Record<string,unknown>,progress?:(value:number)=>void,signal?:AbortSignal):Promise<T>{
    const id=++this.next;
    return new Promise((resolve,reject)=>{
      if(signal?.aborted){reject(new DOMException('Cancelled','AbortError'));return;}
      const abort=()=>{this.worker.postMessage({operation:'cancel',target:id});this.pending.delete(id);reject(new DOMException('Cancelled','AbortError'));};
      signal?.addEventListener('abort',abort,{once:true});
      this.pending.set(id,{resolve,reject,progress,cleanup:()=>signal?.removeEventListener('abort',abort)});
      this.worker.postMessage({id,operation,...args});
    });
  }
  scan(file:File,progress?:(value:number)=>void,signal?:AbortSignal){return this.call<PrivateManifest>('scan',{file},progress,signal);}
  seal(manifest:PrivateManifest,key:string){return this.call<string>('seal',{manifest,key});}
  open(metadata:string,key:string){return this.call<PrivateManifest>('open',{metadata,key});}
  encrypt(file:File,offset:number,size:number,signal?:AbortSignal){return this.call<ArrayBuffer>('encrypt',{file,offset,size},undefined,signal);}
}
export const cryptoClient=new CryptoClient();
export function randomKey(){return Array.from(crypto.getRandomValues(new Uint8Array(32)),n=>n.toString(16).padStart(2,'0')).join('');}
