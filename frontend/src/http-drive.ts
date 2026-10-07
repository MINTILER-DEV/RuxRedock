import type {Account,Drive,Entry,Folder,Requirements,UploadInput,Version,VersionManifest} from './types';
export class HttpError extends Error {constructor(public status:number,public details:any,message:string){super(message);}}
export class HttpDrive implements Drive {
  readonly demo=false;
  constructor(private token:string){}
  private async request<T>(path:string,method='GET',body?:unknown,signal?:AbortSignal):Promise<T>{
    const response=await fetch(path,{method,signal,headers:{Authorization:`Bearer ${this.token}`,...(body!==undefined?{'Content-Type':'application/json'}:{})},body:body===undefined?undefined:JSON.stringify(body)});
    if(!response.ok){const error=await response.json().catch(()=>({message:`Request failed (${response.status})`}));throw new HttpError(response.status,error.details,error.message||'Request failed');}
    return response.status===204?undefined as T:response.json();
  }
  me(){return this.request<Account>('/v1/me');}
  async list(parent:string|null){
    const query=`?limit=1000${parent?`&parent_id=${parent}`:''}`;
    const [files,folders]=await Promise.all([this.request<{files:Entry[]}>(`/v1/files${query}`),this.request<{directories:Folder[]}>(`/v1/directories${query}`)]);
    return {...files,...folders};
  }
  folder(name:string,parent:string|null){return this.request<Folder>('/v1/directories','POST',{name,parent_id:parent});}
  async versions(fileId:string){return (await this.request<{versions:Version[]}>(`/v1/files/${fileId}/versions?limit=1000`)).versions;}
  version(fileId:string,versionId:string){return this.request<VersionManifest>(`/v1/files/${fileId}/versions/${versionId}`);}
  ingest(input:UploadInput,signal?:AbortSignal){return this.request<Requirements>('/v1/files','POST',input,signal);}
  async put(versionId:string,objectId:string,payload:ArrayBuffer,signal?:AbortSignal){
    const response=await fetch(`/v1/uploads/${versionId}/blocks/${objectId}`,{method:'PUT',headers:{Authorization:`Bearer ${this.token}`,'Content-Type':'application/octet-stream'},body:payload,signal});
    if(!response.ok){const error=await response.json().catch(()=>({message:'Block upload failed'}));throw new HttpError(response.status,error.details,error.message);}
  }
  async complete(versionId:string,signal?:AbortSignal){await this.request(`/v1/uploads/${versionId}/complete`,'POST',{},signal);}
  async cancel(versionId:string){await this.request(`/v1/uploads/${versionId}`,'DELETE');}
  async block(objectId:string,signal?:AbortSignal){const response=await fetch(`/v1/blocks/${objectId}`,{headers:{Authorization:`Bearer ${this.token}`},signal});if(!response.ok)throw new Error('Could not read encrypted block');return response.arrayBuffer();}
  async rename(id:string,name:string,folder:boolean){await this.request(`/v1/${folder?'directories':'files'}/${id}`,'PATCH',{name});}
  async remove(id:string,folder:boolean){await this.request(`/v1/${folder?'directories':'files'}/${id}`,'DELETE');}
}
