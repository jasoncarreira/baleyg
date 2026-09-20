import http from 'node:http';
import fs from 'node:fs';
import { fixturePath } from './paths.mjs';
const allowed=new Set(['viewer.html','feature-factory.graph.json','fixture.graph.json','edge-fixture.graph.json']);
const base=new URL('./',import.meta.url);
http.createServer((req,res)=>{
  const name=new URL(req.url,'http://127.0.0.1').pathname.slice(1)||'viewer.html';
  if(name==='favicon.ico'){res.writeHead(204);res.end();return;}
  if(!allowed.has(name)){res.writeHead(404);res.end();return;}
  try { const data=fs.readFileSync(name === 'viewer.html' ? new URL(name,base) : fixturePath(name));res.writeHead(200,{'Content-Type':name.endsWith('.json')?'application/json':'text/html; charset=utf-8','Cache-Control':'no-store','X-Content-Type-Options':'nosniff'});res.end(data); }
  catch {res.writeHead(404);res.end('Missing saved graph fixture.');}
}).listen(8874,'127.0.0.1',()=>console.log('Viewer: http://127.0.0.1:8874/viewer.html'));
