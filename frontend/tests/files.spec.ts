import {test,expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';

test('file list, folders, search, stars, views, and rename',async({page})=>{
  await page.goto('/');await expect(page.getByRole('button',{name:'Project brief.md',exact:true})).toBeVisible();
  await expect(page.getByRole('table')).toBeVisible();
  await page.getByLabel('Search files').fill('budget');await expect(page.getByRole('button',{name:'Q4 budget.csv',exact:true})).toBeVisible();await expect(page.getByRole('button',{name:'Project brief.md',exact:true})).toHaveCount(0);
  await page.getByLabel('Clear search').click();
  await page.getByLabel('Actions for Project brief.md').click();await page.getByRole('menuitem',{name:'Add to starred'}).click();
  await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Starred',exact:true}).click();await expect(page.getByRole('button',{name:'Project brief.md',exact:true})).toBeVisible();
  await page.getByLabel('Grid view').click();await expect(page.locator('.file-card')).toHaveCount(1);await page.getByLabel('List view').click();
  await page.getByRole('button',{name:'My files',exact:false}).first().click();
  await page.getByRole('button',{name:'New folder',exact:true}).click();await page.getByLabel('Folder name').fill('Test folder');await page.getByRole('dialog').getByRole('button',{name:'Create folder',exact:true}).click();
  await page.getByRole('button',{name:'Test folder',exact:true}).last().click();await expect(page.getByRole('heading',{name:'Test folder',exact:false})).toBeVisible();
  await page.getByRole('button',{name:'My files',exact:true}).last().click();
  await page.getByLabel('Actions for Test folder').click();await page.getByRole('menuitem',{name:'Rename',exact:true}).click();await page.getByLabel('Name',{exact:true}).fill('Renamed folder');await page.getByRole('button',{name:'Save name'}).click();await expect(page.getByRole('button',{name:'Renamed folder',exact:true}).last()).toBeVisible();
  await page.getByLabel('Actions for Renamed folder').click();await page.getByRole('menuitem',{name:'Delete',exact:true}).click();await page.getByRole('button',{name:'Delete permanently'}).click();await expect(page.getByRole('button',{name:'Renamed folder',exact:true})).toHaveCount(0);
});

test('Wasm upload, duplicate reuse, versions, and verified binary download',async({page})=>{
  await page.goto('/');await expect(page.getByRole('button',{name:'Getting started.txt',exact:true})).toBeVisible();
  const data=Buffer.from(Array.from({length:200000},(_,i)=>(i*31+(i>>>8)*17)%256));
  await page.locator('input[type=file]').setInputFiles({name:'roundtrip.bin',mimeType:'application/octet-stream',buffer:data});
  await expect(page.locator('.transfer-toast')).toContainText('Uploaded');
  await expect(page.getByRole('button',{name:'roundtrip.bin',exact:true})).toBeVisible();await expect(page.getByRole('button',{name:'Download file',exact:true})).toBeEnabled();
  await page.locator('input[type=file]').setInputFiles({name:'roundtrip.bin',mimeType:'application/octet-stream',buffer:data});
  await expect(page.locator('.transfer-toast')).toContainText('Uploaded');
  await expect(page.locator('.transfer-toast')).toContainText('reused');
  await page.getByRole('button',{name:'roundtrip.bin',exact:true}).click();await page.getByRole('button',{name:'View version history'}).click();await expect(page.locator('.version-item')).toHaveCount(2);
  const downloading=page.waitForEvent('download');await page.getByRole('button',{name:'Download file',exact:true}).click();const downloaded=await downloading;expect(await readFile((await downloaded.path())!)).toEqual(data);
  await page.reload();await expect(page.getByRole('button',{name:'roundtrip.bin',exact:true})).toBeVisible();
});

test('responsive file list has no horizontal overflow and navigation opens',async({page})=>{
  await page.setViewportSize({width:390,height:844});await page.goto('/');await expect(page.getByRole('button',{name:'Getting started.txt',exact:true})).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBeTruthy();
  await page.getByLabel('Toggle navigation').click();await page.getByRole('navigation',{name:'Folders'}).getByRole('button',{name:'Projects',exact:true}).click();await expect(page.getByRole('heading',{name:'Projects',exact:false})).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBeTruthy();
});

test('browser Wasm matches Python PoC crypto and chunk boundaries',async({page})=>{
  await page.goto('/');const vectors=JSON.parse(await readFile('../tests/fixtures/poc-vectors.json','utf8'));
  const result=await page.evaluate(async vectors=>{
    const path='/src/wasm/ruxredock_wasm.js';const wasm=await import(/* @vite-ignore */path);await wasm.default();
    const cryptoResults=vectors.crypto.map((vector:any)=>{const plain=Uint8Array.from(atob(vector.plaintext),c=>c.charCodeAt(0));const payload=wasm.encrypt(plain);return {fingerprint:wasm.hash(plain),object_id:wasm.hash(payload),payload:btoa(String.fromCharCode(...payload)),plain:Array.from(wasm.decrypt(payload,vector.fingerprint,vector.object_id))};});
    let state=vectors.chunking.seed;const data=new Uint8Array(vectors.chunking.size);for(let i=0;i<data.length;i++){state=(Math.imul(state,1664525)+1013904223)>>>0;data[i]=state>>>24;}
    const chunker=new wasm.Chunker(2048,8192,32768,64);const cuts=Array.from(chunker.feed(data));chunker.free();
    return {cryptoResults,cuts};
  },vectors);
  for(let i=0;i<vectors.crypto.length;i++){expect(result.cryptoResults[i].fingerprint).toBe(vectors.crypto[i].fingerprint);expect(result.cryptoResults[i].object_id).toBe(vectors.crypto[i].object_id);expect(result.cryptoResults[i].payload).toBe(vectors.crypto[i].payload);expect(Buffer.from(result.cryptoResults[i].plain)).toEqual(Buffer.from(vectors.crypto[i].plaintext,'base64'));}
  expect(result.cuts).toEqual(vectors.chunking.cuts);
});
