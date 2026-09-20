import { fixturePath } from './paths.mjs';
import http from 'node:http';import fs from 'node:fs';import path from 'node:path';import {fileURLToPath} from 'node:url';
const root=fs.realpathSync(fixturePath('outputs/hard-v1/review'));
http.createServer((req,res)=>{
 try {
  let p=new URL(req.url,'http://127.0.0.1').pathname;
  if(p==='/favicon.ico'){res.writeHead(204);res.end();return;}
  if(!['GET','HEAD'].includes(req.method))throw new Error();
  if(p==='/')p='/index.html';
  if(!p.endsWith('.html'))throw new Error();
  const file=fs.realpathSync(path.resolve(root,'.'+decodeURIComponent(p))),relative=path.relative(root,file);
  if(relative.startsWith('..')||path.isAbsolute(relative)||!fs.statSync(file).isFile())throw new Error();
  res.writeHead(200,{'Content-Type':'text/html; charset=utf-8','Cache-Control':'no-store','X-Content-Type-Options':'nosniff'});res.end(req.method==='HEAD'?undefined:fs.readFileSync(file));
 }catch{res.writeHead(404);res.end();}
}).listen(8876,'127.0.0.1',()=>console.log('Hard-question review: http://127.0.0.1:8876/ (HTML only; blind keys are not served)'));
