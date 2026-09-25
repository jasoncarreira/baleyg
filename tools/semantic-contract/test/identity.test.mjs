import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {lookupKey} from '../lookup.mjs';
import {parseJson} from '../json.mjs';
import {digest,syntaxId,occurrenceId,headerHash,siblingGroupHash,sourceManifestHash,identityRegistry,assignOrdinals,assignOccurrenceOrdinals} from '../identity.mjs';
import {canonicalBytes} from '../json.mjs';
const vectors=JSON.parse(readFileSync(new URL('../../../docs/semantic-evidence/id-test-vectors/stable-ids.json',import.meta.url)));
test('IDENTITY.VECTORS all 64 full digests, domains and canonical inputs from descriptors',()=>{
  assert.equal(vectors.cases.length,64);
  for (const item of vectors.cases) {
    const {sourceSet,path,language,ancestors,declaration,siblingHeaders}=item.descriptor;
    const syntax={sourceSet,path,language,ancestors,declaration};
    const headers=siblingHeaders.map(headerHash);
    for (const row of item.digests) {
      const [kind,input,domain]=row.label==='syntax' ? ['syntax',syntax,'baleyg.syntax.v1\0'] : row.label==='sibling-group' ? ['siblingGroup',{headers},'baleyg.sibling-group.v1\0'] : ['header',siblingHeaders[Number(row.label.slice(-1))],'baleyg.header.v1\0'];
      assert.equal(Buffer.from(domain).toString('hex'),row.domainHex,item.caseId);
      assert.equal(canonicalBytes(input).toString('hex'),row.inputHex,item.caseId);
      assert.equal(digest(kind,input),row.sha256,item.caseId);
    }
    assert.equal(syntaxId(syntax),item.expected.stableId,item.caseId);
    assert.equal(item.expected.anchor.syntaxId,syntaxId(syntax),item.caseId);
    assert.deepEqual(item.expected.anchor.document,{sourceSetId:sourceSet,language,path},item.caseId);
    assert.equal(item.expected.anchor.capturedRevisionId,item.descriptor.revisionId,item.caseId);
    assert.equal(item.expected.anchor.siblingCount,siblingHeaders.length,item.caseId);
    assert.equal(item.expected.anchor.identicalHeaderCount,headers.filter(hash=>hash===headerHash(item.descriptor.header)).length,item.caseId);
    assert.equal(headerHash(item.descriptor.header),item.expected.anchor.headerHash,item.caseId);
    assert.equal(siblingGroupHash(headers),item.expected.anchor.siblingGroupHash,item.caseId);
  }
});
test('IDENTITY.OCCURRENCE independent canonical, domain, full hash and emitted vectors',()=>{
  const ownerSyntaxId=vectors.cases[0].expected.stableId;
  // These fixtures were computed from the documented byte input, not digest().
  const expected=[
    ['r1','call',0,'5a3261565d80529699c7a66174cf9c038cbcc4d01874ae15512921ed51d0fdea'],
    ['r2','call',0,'661ef37d50792c7983348d12c2e3dc05a4dfbe035bebb656431e7e9f5e228d3f'],
    ['r1','reference',0,'f9c4ea01eac566e46182df0c7d2637f4fb68c4c55d3aa52ea5833d83028398b6'],
    ['r1','call',1,'fbcb1a9e5c48eab2080c5214c9d7728ba354bc919e753249c6da073d2bc36dbe']
  ];
  assert.equal(Buffer.from('baleyg.occurrence.v1\0').toString('hex'),'62616c6579672e6f6363757272656e63652e763100');
  for (const [revisionId,kind,ordinal,full] of expected) {
    const input={revisionId,ownerSyntaxId,kind,ordinal};
    const literal=`{"kind":"${kind}","ordinal":${ordinal},"ownerSyntaxId":"${ownerSyntaxId}","revisionId":"${revisionId}"}`;
    assert.equal(canonicalBytes(input).toString('hex'),Buffer.from(literal).toString('hex'));
    assert.equal(digest('occurrence',input),full);
    assert.equal(occurrenceId(input),`occ:v1:${full.slice(0,32)}`);
  }
  assert.equal(new Set(expected.map(row=>row[3].slice(0,32))).size,4);
});
test('IDENTITY.COLLISION retained revisions and distinct full hashes sharing a prefix',()=>{
  const input={sourceSet:'core',path:'src/A.java',language:'java',ancestors:[],declaration:vectors.cases[0].descriptor.declaration};
  const prefix='a'.repeat(32), registry=identityRegistry((_,row)=>prefix+(row.path==='src/B.java'?'b':'c').repeat(32));
  assert.equal(registry.register('syntax',input),registry.register('syntax',structuredClone(input)));
  assert.throws(()=>registry.register('syntax',{...input,path:'src/B.java'}),/IDENTITY.COLLISION/);
  const occurrences=identityRegistry((_,row)=>prefix+(row.revisionId==='r2'?'b':'c').repeat(32));
  const item={revisionId:'r1',ownerSyntaxId:syntaxId(input),kind:'call',ordinal:0};
  assert.throws(()=>{occurrences.register('occurrence',item);occurrences.register('occurrence',{...item,revisionId:'r2'});},/IDENTITY.COLLISION/);
  assert.throws(()=>syntaxId({...input,nativeId:'ignored'}),/unknown field/);
  assert.throws(()=>syntaxId({...input,lookupKey:'normalized'}),/unknown field/);
});
test('IDENTITY.ORDINAL immutable document, parent, exact name and signature scopes',()=>{
  const common={sourceSetId:'core',documentPath:'a.rs',revisionId:'r1',container:[],kind:'function',name:'x',signature:null};
  const a={...common,range:{start:8,end:9},nativeId:'node-1'};
  const b={...common,range:{start:2,end:3},nativeId:'node-2'};
  const different=[
    {...a,documentPath:'b.rs'}, {...a,revisionId:'r2'}, {...a,sourceSetId:'other'},
    {...a,container:[{kind:'type',name:'Parent',signature:null,ordinal:0}]},
    {...a,name:null}, {...a,name:'é'}, {...a,name:'é'},
    {...a,kind:'method',signature:{parameterTypes:['int'],typeParameterCount:0,variadic:false}},
    {...a,kind:'method',signature:{parameterTypes:['long'],typeParameterCount:0,variadic:false}}
  ];
  const ordinals=assignOrdinals([a,b,...different]);
  assert.equal(ordinals.get(b),0);assert.equal(ordinals.get(a),1);
  for(const row of different) assert.equal(ordinals.get(row),0);
  assert.throws(()=>assignOrdinals([a,{...a,lookupKey:'different',nativeId:'other'}]),/IDENTITY.ORDINAL/);
  assert.throws(()=>assignOrdinals([{container:[],kind:'function',name:'x',signature:null,range:{start:0,end:1}}]),/invalid snapshot/);
  const top={sourceSet:'core',path:'a.rs',language:'rust',ancestors:[],declaration:{kind:'function',name:'x',signature:null,ordinal:0}};
  assert.equal(syntaxId(top),syntaxId({...top,ancestors:[]}));
  assert.notEqual(syntaxId(top),syntaxId({...top,ancestors:[{kind:'module',name:null,signature:null,ordinal:0}]}));
  assert.notEqual(syntaxId({...top,declaration:{...top.declaration,name:'é'}}),syntaxId({...top,declaration:{...top.declaration,name:lookupKey('rust','é')}}));
});
test('IDENTITY.MANIFEST pinned empty and populated bytes, sorted and sensitive',()=>{
  const row={document:{sourceSetId:'core',language:'java',path:'A.java'},contentHash:'a'.repeat(64)};
  assert.equal(sourceManifestHash([]),'4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945');
  assert.equal(sourceManifestHash([row]),'7a71de35c8584c011a3bf5dcaf9940da12180667fb49d1261d4a1e9ca094c5b5');
  assert.notEqual(sourceManifestHash([row]),sourceManifestHash([{...row,contentHash:'b'.repeat(64)}]));
  assert.throws(()=>sourceManifestHash([row,row]),/IDENTITY.MANIFEST/);
  const rust={document:{...row.document,language:'rust',path:'z.rs'},contentHash:row.contentHash};
  assert.throws(()=>sourceManifestHash([rust,row]),/unsorted/);
  assert.throws(()=>sourceManifestHash([row,{...row,document:{...row.document,path:'0.java'}}]),/unsorted/);
  assert.match(sourceManifestHash([row,rust]),/^[a-f0-9]{64}$/);
});
test('IDENTITY.CANONICAL every control and closed nested input',()=>{
  const controls=Array.from({length:32},(_,n)=>`\\u00${n.toString(16).padStart(2,'0')}`).join('');
  assert.equal(canonicalBytes(Array.from({length:32},(_,n)=>String.fromCharCode(n)).join('')).toString('utf8'),`"${controls}"`);
  assert.equal(canonicalBytes({z:'"\\',a:'é😀\u2028\u2029'}).toString('utf8'),'{"a":"é😀\u2028\u2029","z":"\\"\\\\"}');
  assert.equal(canonicalBytes('é').toString('hex'),'22c3a922');
  assert.equal(canonicalBytes({b:0,a:null}).toString('hex'),Buffer.from('{"a":null,"b":0}').toString('hex'));
  for (const bad of [-0,1.5,-1,Number.MAX_SAFE_INTEGER+1]) assert.throws(()=>canonicalBytes(bad),/JSON.CANONICAL/);
  for (const bad of ['{"a":1,"a":2}','-0','1.5','-1']) assert.throws(()=>parseJson(bad),/JSON.INTAKE/);
  const syntax={sourceSet:'core',path:'a.rs',language:'rust',ancestors:[],declaration:{kind:'function',name:'a',signature:null,ordinal:0}};
  assert.throws(()=>syntaxId({...syntax,declaration:{...syntax.declaration,extra:1}}));
  assert.throws(()=>syntaxId({...syntax,ancestors:[{...syntax.declaration,extra:1}]}));
  assert.equal(canonicalBytes([]).toString('hex'),'5b5d');
});
test('IDENTITY.OCCURRENCE_ORDINAL owner and kind isolate measured namespaces',()=>{
  const make=(ownerSyntaxId,kind,start)=>({revisionId:'r1',ownerSyntaxId,kind,range:{start,end:start+1}});
  const a=make(vectors.cases[0].expected.stableId,'call',5);
  const b=make(a.ownerSyntaxId,'call',1), c=make(a.ownerSyntaxId,'reference',5);
  const d=make(vectors.cases[2].expected.stableId,'call',5);
  const ordinals=assignOccurrenceOrdinals([a,b,c,d]);
  assert.equal(ordinals.get(a),1); assert.equal(ordinals.get(b),0);
  assert.equal(ordinals.get(c),0); assert.equal(ordinals.get(d),0);
  assert.throws(()=>assignOccurrenceOrdinals([a,{...a}]),/duplicate native range/);
});
