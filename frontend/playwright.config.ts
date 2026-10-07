import {defineConfig} from '@playwright/test';
const baseURL=process.env.BROWSER_BASE_URL||'http://127.0.0.1:5173';
export default defineConfig({testDir:'./tests',timeout:60000,use:{baseURL,viewport:{width:1440,height:960},headless:true},webServer:process.env.BROWSER_BASE_URL?undefined:{command:'npm run dev',url:baseURL,reuseExistingServer:!process.env.CI,timeout:30000},reporter:'list'});
