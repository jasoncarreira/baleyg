import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import Parser from 'tree-sitter';
import JavaScript from 'tree-sitter-javascript';
import { scip } from '@sourcegraph/scip-typescript/dist/src/scip.js';

const sha = text => crypto.createHash('sha256').update(text).digest('hex');
const field = (node, name) => node.childForFieldName(name);
const unwrap = node => { while(node?.type==='parenthesized_expression'&&node.namedChildCount===1) node=node.namedChildren[0]; return node; };
const functionTypes = new Set(['function_declaration', 'function_expression', 'arrow_function', 'generator_function_declaration', 'generator_function', 'method_definition']);
const loopTypes = new Set(['for_statement', 'for_in_statement', 'while_statement', 'do_statement']);
function positionRange(range) {
  return range.length === 3 ? [range[0], range[1], range[0], range[2]] : range;
}
function sameSpan(node, range) {
  const [sr, sc, er, ec] = positionRange(range);
  return node.startPosition.row === sr && node.startPosition.column === sc && node.endPosition.row === er && node.endPosition.column === ec;
}
const sortId = (a, b) => a.id.localeCompare(b.id, 'en');
const scoped = (symbol, file) => symbol.startsWith('local ') ? `local:${file}:${symbol}` : symbol;

export function extract(inputRoot, scipPath, manifest) {
  const start = performance.now();
  const index = scip.Index.deserialize(fs.readFileSync(scipPath)).toObject();
  const parser = new Parser(); parser.setLanguage(JavaScript);
  const indexedDocs = new Map(index.documents.map(doc => [doc.relative_path,doc]));
  const sourcePaths = fs.readdirSync(inputRoot,{recursive:true,withFileTypes:true})
    .filter(e=>e.isFile() && e.name.endsWith('.js'))
    .map(e=>path.relative(inputRoot,path.join(e.parentPath,e.name)))
    .filter(p=>!p.split(path.sep).includes('node_modules')).sort();
  const files = sourcePaths.map(relative => {
    const text=fs.readFileSync(path.join(inputRoot,relative),'utf8');
    return {path:relative,hash:sha(text),text};
  });
  const docs=files.map(f=>indexedDocs.get(f.path)??{relative_path:f.path,occurrences:[]});
  const current = Object.fromEntries(files.map(f=>[f.path,f.hash]));
  for(const config of ['package.json','tsconfig.json','jsconfig.json']) {
    const full=path.join(inputRoot,config);
    if(fs.existsSync(full)) current[config]=sha(fs.readFileSync(full));
  }
  // Conservative spike rule: ANY input drift invalidates semantic resolution for the whole index.
  // File-local invalidation without reverse dependencies would retain stale callers in other files.
  const changedFiles=[...new Set([...Object.keys(manifest),...Object.keys(current)])]
    .filter(p=>manifest[p]!==current[p]).sort();
  const semanticFresh = changedFiles.length === 0;
  const nodes=[], calls=[], regions=[], references=[];
  let parseErrors=0, occurrences=0, referenceEnclosingRanges=0, definitionEnclosingRanges=0;
  const fileMaps = new Map();
  for (let i=0; i<docs.length; i++) {
    const doc=docs[i], file=files[i], tree=parser.parse(file.text);
    if(tree.rootNode.hasError) parseErrors++;
    const occ=doc.occurrences;
    occurrences+=occ.length;
    referenceEnclosingRanges+=occ.filter(o=>!(o.symbol_roles&1) && o.enclosing_range.length).length;
    definitionEnclosingRanges+=occ.filter(o=>(o.symbol_roles&1) && o.enclosing_range.length).length;
    const symbolsAt = (n, definitions=false) => n ? [...new Set(occ.filter(o=>!!(o.symbol_roles&1)===definitions && sameSpan(n,o.range)).map(o=>scoped(o.symbol,file.path)))] : [];
    const fnMap=new Map();
    const moduleId=`module:${file.path}`;
    nodes.push({id:moduleId,name:file.path,kind:'module',path:file.path,line:1,endLine:tree.rootNode.endPosition.row+1});
    function declarations(node, parentFunction=null) {
      let current=parentFunction;
      if (functionTypes.has(node.type)) {
        let nameNode=field(node,'name');
        if(!nameNode && ['variable_declarator','pair'].includes(node.parent?.type)) nameNode=field(node.parent,node.parent.type==='pair'?'key':'name');
        const ids=symbolsAt(nameNode,true);
        const id=semanticFresh && ids.length===1 ? ids[0] : `syntax:${file.path}:${node.startIndex}:${node.type}`;
        const name=nameNode?.text ?? `<callback@${node.startPosition.row+1}:${node.startPosition.column+1}>`;
        current={id,name,kind:node.type==='method_definition'?'method':'function',accessor:node.type==='method_definition'&&node.children.some(c=>['get','set'].includes(c.type)),path:file.path,line:node.startPosition.row+1,endLine:node.endPosition.row+1,parent:parentFunction?.id??moduleId,provenance:semanticFresh&&ids.length===1?'scip+treesitter':'treesitter'};
        nodes.push(current); fnMap.set(node.id,current);
      }
      for (const child of node.namedChildren) declarations(child,current);
    }
    declarations(tree.rootNode);
    function region(node, kind, label, context, owner) {
      const id=`region:${file.path}:${node.startIndex}:${kind}`;
      regions.push({id,kind,label,parent:context.at(-1)??null,path:file.path,line:node.startPosition.row+1,endLine:node.endPosition.row+1,owner});
      return [...context,id];
    }
    function walk(node, caller=moduleId, context=[]) {
      let skipName=null;
      if(fnMap.has(node.id)) {
        // Computed method keys execute while defining the class/object, not when calling the method.
        if(node.type==='method_definition') {skipName=field(node,'name');if(skipName) walk(skipName,caller,context);}
        caller=fnMap.get(node.id).id;context=[];
      }
      if(node.type==='ternary_expression') {
        const condition=field(node,'condition'); if(condition) walk(condition,caller,context);
        const yes=field(node,'consequence'),no=field(node,'alternative');
        if(yes) walk(yes,caller,region(yes,'conditional-true',`when ${condition?.text}`,context,caller));
        if(no) walk(no,caller,region(no,'conditional-false',`unless ${condition?.text}`,context,caller));
        return;
      }
      if(node.type==='binary_expression') {
        const op=field(node,'operator')?.text;
        if(['&&','||','??'].includes(op)) {
          const left=field(node,'left'),right=field(node,'right');
          if(left) walk(left,caller,context);
          if(right) walk(right,caller,region(right,'short-circuit',`${op} RHS guarded by ${left?.text}`,context,caller));
          return;
        }
      }
      if(node.type==='if_statement') {
        const cond=field(node,'condition'); if(cond) walk(cond,caller,context);
        const yes=field(node,'consequence'),no=field(node,'alternative');
        if(yes) walk(yes,caller,region(node,'if',`if ${cond?.text??''}`,context,caller));
        if(no) walk(no,caller,region(no,'else',`else of line ${node.startPosition.row+1}`,context,caller));
        return;
      }
      let skipLoopInit=null;
      if(loopTypes.has(node.type)) {
        skipLoopInit=field(node,node.type==='for_in_statement'?'right':'initializer');
        if(skipLoopInit) walk(skipLoopInit,caller,context);
        context=region(node,'loop',node.text.split('{')[0].trim().slice(0,160),context,caller);
      }
      if(['try_statement','catch_clause','finally_clause','switch_statement','switch_case','switch_default','ternary_expression'].includes(node.type)) context=region(node,node.type,node.text.split(/[\n{]/)[0].slice(0,140),context,caller);
      if(node.type==='call_expression'||node.type==='new_expression') {
        const callee=unwrap(field(node,node.type==='new_expression'?'constructor':'function'));
        const token=callee?.type==='member_expression'?field(callee,'property'):callee;
        const candidates=semanticFresh?symbolsAt(token):[];
        const args=field(node,'arguments');
        const cb=[];
        if(args) for(const rawArg of args.namedChildren) {
          const arg=unwrap(rawArg);
          if(fnMap.has(arg.id)) cb.push(fnMap.get(arg.id).id);
          else if(['identifier','member_expression'].includes(arg.type)) cb.push(...(semanticFresh?symbolsAt(arg.type==='member_expression'?field(arg,'property'):arg):[]));
        }
        calls.push({id:`call:${file.path}:${node.startIndex}`,caller,calleeText:callee?.text??'<unknown>',target:null,resolution:'unresolved',candidateSymbols:candidates,callbackArguments:cb,path:file.path,line:node.startPosition.row+1,column:node.startPosition.column+1,offset:node.startIndex,ordinal:0,regions:[...context],syntax:node.type,provenance:{callsite:'treesitter',resolution:semanticFresh?'scip':'stale-index'}});
      }
      for (const child of node.namedChildren) if(child.id!==skipName?.id && child.id!==skipLoopInit?.id) walk(child,caller,context);
    }
    walk(tree.rootNode);
    // Retain actual SCIP references separately: this is deliberately NOT a call list.
    for(const o of occ) if(!(o.symbol_roles&1)) references.push({path:file.path,range:o.range,symbol:scoped(o.symbol,file.path),enclosingRange:o.enclosing_range,stale:!semanticFresh});
    fileMaps.set(file.path,{tree,fnMap});
  }
  const byId=new Map(nodes.map(n=>[n.id,n]));
  for(const c of calls) {
    const internal=c.candidateSymbols.filter(s=>byId.has(s)&&byId.get(s).kind!=='module'&&!byId.get(s).accessor);
    const callable=c.candidateSymbols.filter(s=>s.endsWith('().')&&!byId.get(s)?.accessor);
    if(internal.length===1 && c.candidateSymbols.length===1) {c.target=internal[0];c.resolution='internal';}
    else if(c.candidateSymbols.length>1) c.resolution='ambiguous';
    else if(callable.length===1) {c.target=callable[0];c.resolution='external';}
    c.callbackArguments=c.callbackArguments.filter(s=>byId.has(s)&&byId.get(s).kind!=='module'&&!byId.get(s).accessor);
  }
  calls.sort((a,b)=>a.path.localeCompare(b.path,'en')||a.offset-b.offset);
  const ordinals=new Map();
  for(const c of calls) {c.ordinal=(ordinals.get(c.caller)??0)+1;ordinals.set(c.caller,c.ordinal);}
  const stats={files:files.length,nodes:nodes.length,calls:calls.length,internal:calls.filter(c=>c.resolution==='internal').length,external:calls.filter(c=>c.resolution==='external').length,unresolved:calls.filter(c=>c.resolution==='unresolved').length,ambiguous:calls.filter(c=>c.resolution==='ambiguous').length,parseErrors,occurrences,referenceEnclosingRanges,definitionEnclosingRanges,semanticFresh,changedFiles};
  const graph={schemaVersion:1,scope:'Static lexical call sites, NOT execution order. Callback bodies are separate functions. No points-to analysis.',indexTool:index.metadata.tool_info,files,nodes:nodes.sort(sortId),calls,regions:regions.sort(sortId),references,stats};
  return {graph,elapsedMs:performance.now()-start};
}

if(process.argv[1]===fileURLToPath(import.meta.url)) {
  const [input,scipFile,manifestFile,output]=process.argv.slice(2);
  if(!output) throw new Error('usage: node extract.mjs INPUT SCIP HASH_MANIFEST OUTPUT');
  const {graph,elapsedMs}=extract(input,scipFile,JSON.parse(fs.readFileSync(manifestFile,'utf8')));
  fs.writeFileSync(output,JSON.stringify(graph,null,2)+'\n');
  console.log(JSON.stringify({output,elapsedMs,...graph.stats},null,2));
}
