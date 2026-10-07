export type Folder={id:string;name:string;parent_id:string|null};
export type Chunk={object_id:string;size:number};
export type PrivateChunk=Chunk&{fingerprint:string;offset:number};
export type PrivateManifest={version:1;name:string;size:number;sha256:string;chunks:PrivateChunk[];chunking:{min_size:number;avg_size:number;max_size:number;window_size:number}};
export type Entry={id:string;name:string;parent_id:string|null;size:number;modified:string;version_count:number;status:'pending'|'ready'};
export type Version={id:string;size:number;status:'pending'|'ready';created_at:string};
export type VersionManifest={file_id:string;version_id:string;size:number;status:'pending'|'ready';client_metadata:string;chunks:Chunk[]};
export type Requirements={file_id:string;version_id:string;missing:boolean[];missing_chunks:Chunk[];upload_bytes:number;status:'pending'|'ready'};
export type UploadInput={name:string;parent_id:string|null;size:number;client_metadata:string;chunks:Chunk[]};
export type Account={id:string;name:string;used_bytes:number;quota_bytes:number};
export type Transfer={id:string;name:string;kind:'upload'|'download';stage:string;percent:number;status:'running'|'done'|'error'|'cancelled';saved?:number;error?:string;controller:AbortController};
export interface Drive {
  readonly demo:boolean;
  me():Promise<Account>;
  list(parent:string|null):Promise<{files:Entry[];directories:Folder[]}>;
  folder(name:string,parent:string|null):Promise<Folder>;
  versions(fileId:string):Promise<Version[]>;
  version(fileId:string,versionId:string):Promise<VersionManifest>;
  ingest(input:UploadInput,signal?:AbortSignal):Promise<Requirements>;
  put(versionId:string,objectId:string,payload:ArrayBuffer,signal?:AbortSignal):Promise<void>;
  complete(versionId:string,signal?:AbortSignal):Promise<void>;
  cancel(versionId:string):Promise<void>;
  block(objectId:string,signal?:AbortSignal):Promise<ArrayBuffer>;
  rename(id:string,name:string,folder:boolean):Promise<void>;
  remove(id:string,folder:boolean):Promise<void>;
}
