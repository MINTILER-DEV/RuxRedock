import {test,expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';

test('private server uploads, versions, recovery, and verified download',async({page})=>{
  test.skip(!process.env.BROWSER_API_TOKEN,'Requires scripts/verify-browser-server.sh and real services');
  await page.addInitScript(()=>Object.defineProperty(window,'showSaveFilePicker',{value:undefined,configurable:true}));
  const connect=async()=>{
    await page.getByRole('button',{name:'Connect your server'}).click();
    await page.getByLabel('Access token').fill(process.env.BROWSER_API_TOKEN!);
    await page.getByLabel('Recovery key',{exact:false}).fill('a'.repeat(64));
    await page.getByRole('button',{name:'Connect workspace',exact:true}).click();
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.locator('.workspace-card')).toContainText('Server verification');
  };
  await page.goto('/');await expect(page.getByRole('button',{name:'Project brief.md',exact:true})).toBeVisible();await connect();
  await page.getByRole('button',{name:'New folder',exact:true}).click();await page.getByLabel('Folder name').fill('Verified files');await page.getByRole('dialog').getByRole('button',{name:'Create folder',exact:true}).click();
  await page.getByRole('button',{name:'Verified files',exact:true}).last().click();
  const data=Buffer.from(Array.from({length:400000},(_,i)=>(i*37+(i>>>7)*23)%256));
  await page.locator('input[type=file]').setInputFiles({name:'private-roundtrip.bin',mimeType:'application/octet-stream',buffer:data});
  await expect(page.locator('.transfer-toast')).toContainText('Uploaded');await expect(page.getByRole('button',{name:'private-roundtrip.bin',exact:true})).toBeVisible();
  await page.locator('input[type=file]').setInputFiles({name:'private-roundtrip.bin',mimeType:'application/octet-stream',buffer:data});
  await expect(page.locator('.transfer-toast')).toContainText('Uploaded');await expect(page.locator('.transfer-toast')).toContainText('reused');
  await page.getByRole('button',{name:'View version history'}).click();await expect(page.locator('.version-item')).toHaveCount(2);
  const downloading=page.waitForEvent('download');await page.getByRole('button',{name:'Download file',exact:true}).click();expect(await readFile((await (await downloading).path())!)).toEqual(data);
  await page.reload();await expect(page.getByRole('button',{name:'Project brief.md',exact:true})).toBeVisible();await connect();
  await page.getByRole('button',{name:'Verified files',exact:true}).last().click();await page.getByRole('button',{name:'private-roundtrip.bin',exact:true}).click();
  const recovered=page.waitForEvent('download');await page.getByRole('button',{name:'Download file',exact:true}).click();expect(await readFile((await (await recovered).path())!)).toEqual(data);
  // Exercise the streaming branch and ensure the picker still has user activation.
  await page.evaluate(()=>{
    const state={active:false,name:'',closed:false,aborted:false,parts:[] as number[][]};
    (window as any).streamTest=state;
    Object.defineProperty(window,'showSaveFilePicker',{configurable:true,value:async(options:{suggestedName:string})=>{
      state.active=navigator.userActivation.isActive;state.name=options.suggestedName;
      return {createWritable:async()=>({write:async(bytes:ArrayBuffer)=>state.parts.push(Array.from(new Uint8Array(bytes))),close:async()=>{state.closed=true;},abort:async()=>{state.aborted=true;}})};
    }});
  });
  await page.getByRole('button',{name:'Download file',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).streamTest.closed)).toBe(true);
  const streamed=await page.evaluate(()=>(window as any).streamTest);expect(streamed.active).toBe(true);expect(streamed.name).toBe('private-roundtrip.bin');expect(streamed.aborted).toBe(false);expect(Buffer.from(streamed.parts.flat())).toEqual(data);

});
