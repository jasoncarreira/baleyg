import { fixturePath } from './paths.mjs';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root=fs.realpathSync(fixturePath('outputs'));
const file=fs.realpathSync(process.argv[2]??path.join(root,'smoke-review/review.html'));
const rel=path.relative(root,file);
if(rel.startsWith('..')||path.isAbsolute(rel)||path.extname(file)!=='.html') throw new Error('Only a generated HTML review under outputs may be served');
http.createServer((req,res)=>{
 const url=new URL(req.url,'http://127.0.0.1');
 if(url.pathname==='/favicon.ico'){res.writeHead(204);res.end();return;}
 if(!['GET','HEAD'].includes(req.method)||!['/','/review.html'].includes(url.pathname)){res.writeHead(404);res.end();return;}
 res.writeHead(200,{'Content-Type':'text/html; charset=utf-8','Cache-Control':'no-store','X-Content-Type-Options':'nosniff'});
 res.end(req.method==='HEAD'?undefined:fs.readFileSync(file));
}).listen(8875,'127.0.0.1',()=>console.log('Blind review: http://127.0.0.1:8875/review.html (provider key is not served)'));
