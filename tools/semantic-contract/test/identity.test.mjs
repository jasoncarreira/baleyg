import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
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
    assert.equal(headerHash(item.descriptor.header),item.expected.anchor.headerHash,item.caseId);
    assert.equal(siblingGroupHash(headers),item.expected.anchor.siblingGroupHash,item.caseId);
  }
});
test('IDENTITY.OCCURRENCE revision, kind and ordinal have independent full hash inputs',()=>{
  const ownerSyntaxId=vectors.cases[0].expected.stableId;
  const base={revisionId:'r1',ownerSyntaxId,kind:'call',ordinal:0};
  assert.equal(digest('occurrence',base),'5a3261565d80529699c7a66174cf9c038cbcc4d01874ae15512921ed51d0fdea');
  assert.equal(occurrenceId(base),`occ:v1:${digest('occurrence',base).slice(0,32)}`);
  const ids=[base,{...base,revisionId:'r2'},{...base,kind:'reference'},{...base,ordinal:1}].map(occurrenceId);
  assert.equal(new Set(ids).size,4);
});
test('IDENTITY.COLLISION rejects distinct canonical inputs but permits retained identity',()=>{
  const input={sourceSet:'core',path:'src/A.java',language:'java',ancestors:[],declaration:vectors.cases[0].descriptor.declaration};
  const registry=identityRegistry(()=> 'a'.repeat(64));
  assert.equal(registry.register('syntax',input),registry.register('syntax',structuredClone(input)));
  assert.throws(()=>registry.register('syntax',{...input,path:'src/B.java'}),/IDENTITY.COLLISION/);
});
test('IDENTITY.ORDINAL exact sibling signature and ancestor boundaries',()=>{
  const a={container:'root',kind:'method',name:'x',signature:null,range:{start:8,end:9}};
  const b={...a,range:{start:2,end:3}}; const c={...a,signature:{parameterTypes:['int'],typeParameterCount:0,variadic:false}};
  const ordinals=assignOrdinals([a,b,c]);
  assert.equal(ordinals.get(b),0); assert.equal(ordinals.get(a),1); assert.equal(ordinals.get(c),0);
  assert.throws(()=>assignOrdinals([a,{...a}]),/IDENTITY.ORDINAL/);
});
test('IDENTITY.MANIFEST hashes canonical sorted rows without domain',()=>{
  const row={document:{sourceSetId:'core',language:'java',path:'A.java'},contentHash:'a'.repeat(64)};
  assert.match(sourceManifestHash([row]),/^[a-f0-9]{64}$/);
  assert.throws(()=>sourceManifestHash([row,row]),/IDENTITY.MANIFEST/);
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
