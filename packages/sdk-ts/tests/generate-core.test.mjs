import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, statSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compile } from "json-schema-to-typescript";
import { preflightRegistry, validateLocalRefs, memoizeSchemaCompiler,
  createComponentRegistry, writeGeneratedFile, generateContracts } from "../scripts/generate-core.mjs";

const object = (title) => ({ title, type: "object", properties: {}, additionalProperties: false });
function resolvePointer(root, ref) {
  let value=root;
  for(const token of ref==="#"?[]:ref.slice(2).split("/")){
    value=value?.[token.replaceAll("~1","/").replaceAll("~0","~")];
    assert.notEqual(value,undefined,ref);
  }
  return value;
}
// Compare reference graphs coinductively. Only generated pure aliases unwrap;
// $ref siblings remain assertions and are compared, not silently discarded.
function assertEquivalent(original, document, ref) {
  const seen=new WeakMap();
  const pure=(value)=>value&&typeof value==="object"&&!Array.isArray(value)
    && typeof value.$ref==="string"&&Object.keys(value).length===1;
  let actual=resolvePointer(document,ref);
  const rootAliases=new Set();
  while(actual.$ref&&/^#\/components\/schemas\/[^/]+$/.test(actual.$ref)
      &&Object.keys(actual).every((key)=>["$ref","$defs","definitions"].includes(key))){
    if(rootAliases.has(actual.$ref))break;
    rootAliases.add(actual.$ref);
    actual=resolvePointer(document,actual.$ref);
  }
  function compare(old,value) {
    if(!old||!value||typeof old!=="object"||typeof value!=="object")return assert.deepEqual(value,old);
    let peers=seen.get(old);if(!peers){peers=new WeakSet();seen.set(old,peers);}
    if(peers.has(value))return;peers.add(value);
    if(pure(value))return compare(old,resolvePointer(document,value.$ref));
    if(pure(old))return compare(resolvePointer(original,old.$ref),value);
    assert.equal(Array.isArray(value),Array.isArray(old));
    assert.deepEqual(Object.keys(value).sort(),Object.keys(old).sort());
    for(const key of Object.keys(old)){
      if(key==="$ref")compare(resolvePointer(original,old[key]),resolvePointer(document,value[key]));
      else compare(old[key],value[key]);
    }
  }
  compare(original,actual);
}
function registry() {
  return { tools: [{ name: "opencut_example", input_schema: object("Example"), output_schema: object("Response") }],
    jobs: [], domains: [{ domain: "edit", actions: [{ action: "example", operation: "opencut_example" }] }],
    eventSchema: object("Event"), audioStream: { input: object("Stream"), header: object("Header") },
    visualStream: { input: object("PictureRead"), descriptor: object("Picture"), content_type: "application/vnd.opencut.rgba", pixel_format: "rgba8", chunk_bytes: 65536 } };
}
test("preflight rejects incomplete routes, collisions, root titles and non-local or dangling refs", () => {
  preflightRegistry(registry());
  for (const mutate of [
    (r) => r.domains[0].actions.splice(0),
    (r) => r.domains.push({ ...r.domains[0] }),
    (r) => r.tools.push({ ...r.tools[0] }),
    (r) => { r.tools[0].input_schema.title = "invalid_title"; },
    (r) => { r.tools[0].output_schema.$ref = "https://example.test/schema"; },
    (r) => { r.tools[0].input_schema.$ref = "#/$defs/Missing"; },
    (r) => { r.audioStream.header.$ref = "#/~2bad"; },
    (r) => { r.visualStream.descriptor.$ref = "#/$defs/Missing"; },
    (r) => { r.visualStream.chunk_bytes = 131072; },
    (r) => { r.visualStream.content_type = "image/png"; },
    (r) => { delete r.visualStream; },
    (r) => { r.jobs.push({name:"media.a-b",input_schema:object("Job"),result_schema:object("Result")},
      {name:"media.a_b",input_schema:object("Job"),result_schema:object("Result")}); },
  ]) { const r = registry(); mutate(r); assert.throws(() => preflightRegistry(r)); }
});

test("invalid complete registry fails before invoking the compiler or writing artifacts", async () => {
  const directory = mkdtempSync(join(tmpdir(), "opencut-generator-test-"));
  try {
    const r = registry(); r.domains[0].actions = [];
    const runner = join(directory, "schema-runner");
    writeFileSync(runner, `#!/usr/bin/env node\nconst r=${JSON.stringify(r)};\nconst op=process.argv[3];\nconst data=op==='schema'?r.tools:op==='capabilities'?{job_results:r.jobs}:op==='domains'?r.domains:op==='events-schema'?r.eventSchema:op==='visual-preview-schema'?r.visualStream:r.audioStream;\nconsole.log(JSON.stringify({data}));\n`, {mode:0o700});
    let calls = 0;
    await assert.rejects(generateContracts(() => { calls++; throw new Error("must not compile"); },
      ["--binary", runner, "--skip-build"]), /does not cover/);
    assert.equal(calls, 0);
  } finally { rmSync(directory, {recursive:true, force:true}); }
});

test("memo preserves actual compiler bytes and does not merge order-sensitive schemas", async () => {
  const options = {bannerComment:"",style:{singleQuote:true},unknownAny:false};
  const schema = { ...object("Example"), properties:{first:{type:"string"},second:{type:"number"}} };
  let calls = 0;
  const memo = memoizeSchemaCompiler((...args) => { calls++; return compile(...args); });
  const expected = await compile(schema, "Schema", options);
  const values = await Promise.all([memo(schema,"Schema",options),memo(structuredClone(schema),"Schema",options)]);
  assert.deepEqual(values, [expected,expected]); assert.equal(calls,1);
  const reversed = {...schema,properties:{second:{type:"number"},first:{type:"string"}}};
  assert.equal(await memo(reversed,"Schema",options),await compile(reversed,"Schema",options));
  assert.equal(calls,2);
});

test("closed-root sharing preserves recursive refs, escaped definitions and original entry names", () => {
  const schema = {...object("Tree"), properties:{node:{$ref:"#/$defs/a~1b~0c"}},
    $defs:{"a/b~c":{type:"object",properties:{next:{$ref:"#/$defs/a~1b~0c"}}}}};
  validateLocalRefs(schema);
  const {schemas,component} = createComponentRegistry();
  assert.deepEqual(component(schema,"First"),{$ref:"#/components/schemas/First"});
  assert.deepEqual(component(structuredClone(schema),"Second"),{$ref:"#/components/schemas/Second"});
  assert.deepEqual(schemas.Second,{$ref:"#/components/schemas/First",$defs:{"a/b~c":{$ref:"#/components/schemas/First/$defs/a~1b~0c"}}});
  assert.equal(schemas.First.$defs["a/b~c"].properties.next.$ref,"#/components/schemas/First/$defs/a~1b~0c");
  validateLocalRefs({components:{schemas}});
  // Same local ref names with different closed definitions must stay separate.
  const other=structuredClone(schema); other.$defs["a/b~c"].required=["next"];
  component(other,"Third"); assert.ok(schemas.Third.properties);
  assert.throws(()=>component(schema,"First"),/Duplicate/);
  const scoped={...schema,$id:"urn:test:tree"};
  component(scoped,"ScopedOne"); component(scoped,"ScopedTwo");
  assert.ok(schemas.ScopedTwo.properties);
});

test("definition sharing compares the entire reachable closure, cycles, root context and ref siblings", () => {
  const make=(title,last)=>({...object(title),properties:{entry:{$ref:"#/$defs/A"}},$defs:{
    A:{type:"object",properties:{next:{$ref:"#/$defs/B"}}},
    B:{type:"object",properties:{next:{$ref:"#/$defs/A"},last:{enum:[last]}}},
    D:{$ref:"#"},N:{type:"number"},Bound:{$ref:"#/$defs/N",minimum:5}}});
  const first=make("First","x"),same=make("Second","x"),changed=make("Third","y");
  same.$defs.Unrelated={type:"boolean"};changed.$defs.Bound.minimum=6;
  const {schemas,component}=createComponentRegistry();
  for(const [name,schema]of [["First",first],["Second",same],["Third",changed]])component(schema,name);
  assert.equal(schemas.Second.$defs.A.$ref,"#/components/schemas/First/$defs/A");
  assert.equal(schemas.Second.$defs.B.$ref,"#/components/schemas/First/$defs/B");
  assert.ok(schemas.Third.$defs.A.properties);assert.ok(schemas.Third.$defs.B.properties);
  assert.equal(schemas.Second.$defs.Bound.$ref,"#/components/schemas/First/$defs/Bound");
  assert.equal(schemas.Third.$defs.Bound.minimum,6);
  assert.equal(schemas.Second.$defs.D.$ref,"#/components/schemas/Second");
  const document={components:{schemas}};
  for(const [name,schema]of [["First",first],["Second",same],["Third",changed]])assertEquivalent(schema,document,`#/components/schemas/${name}`);
  validateLocalRefs(document);
  // The oracle must reject assertion changes even when the $ref target matches.
  const invalid=structuredClone(document);invalid.components.schemas.Third.$defs.Bound.minimum=5;
  assert.throws(()=>assertEquivalent(changed,invalid,"#/components/schemas/Third"));
  // A recursive root's $ref is an assertion, not a generated alias when
  // siblings exist. Keep those siblings and terminate the self-reference.
  const recursiveRoot={title:"Root",$ref:"#",minimum:5};
  const recursiveRegistry=createComponentRegistry();recursiveRegistry.component(recursiveRoot,"Root");
  const recursiveDocument={components:{schemas:recursiveRegistry.schemas}};
  assertEquivalent(recursiveRoot,recursiveDocument,"#/components/schemas/Root");
  const wrongRoot=structuredClone(recursiveDocument);wrongRoot.components.schemas.Root.minimum=6;
  assert.throws(()=>assertEquivalent(recursiveRoot,wrongRoot,"#/components/schemas/Root"));
  assertEquivalent({$ref:"#"},{components:{schemas:{Cycle:{$ref:"#/components/schemas/Cycle"}}}},"#/components/schemas/Cycle");
});

test("nested definition and descendant-pointer entries remain physical; scoped roots never share definitions", () => {
  const schema={...object("Nested"),properties:{entry:{$ref:"#/$defs/Outer/$defs/a~1b~0c"}},$defs:{
    Outer:{$defs:{"a/b~c":{type:"string"}},type:"object"},Leaf:{type:"number"}}};
  const {schemas,component}=createComponentRegistry();
  component(schema,"First");component(structuredClone(schema),"Second");
  const document={components:{schemas}};
  assert.deepEqual(resolvePointer(document,"#/components/schemas/Second/$defs/Outer/$defs/a~1b~0c"),{type:"string"});
  assert.equal(schemas.Second.$defs.Leaf.$ref,"#/components/schemas/First/$defs/Leaf");
  assertEquivalent(schema,document,"#/components/schemas/Second");
  for(const [index,key]of ["$id","$anchor","$dynamicAnchor","$recursiveAnchor"].entries()){
    const scoped=structuredClone(schema);scoped.$defs.Leaf[key]=key==="$recursiveAnchor"?true:"test";
    component(scoped,`Scoped${index}`);assert.equal(schemas[`Scoped${index}`].$defs.Leaf.type,"number");
  }
  const nestedTarget={...object("Pointer"),properties:{entry:{$ref:"#/$defs/Outer/properties/value"}},
    $defs:{Outer:{type:"object",properties:{value:{type:"integer"}}}}};
  component(nestedTarget,"PointerOne");component(structuredClone(nestedTarget),"PointerTwo");
  assert.deepEqual(resolvePointer(document,"#/components/schemas/PointerTwo/$defs/Outer/properties/value"),{type:"integer"});
});

test("all generated operation roots remain equivalent and every original local definition link resolves", () => {
  const tools=JSON.parse(readFileSync(new URL("../../contract-fixtures/tools.generated.json",import.meta.url),"utf8"));
  const document=JSON.parse(readFileSync(new URL("../../contract-fixtures/openapi.generated.json",import.meta.url),"utf8"));
  const pascal=(name)=>name.split(/[^a-zA-Z0-9]+/).filter(Boolean).map(p=>p[0].toUpperCase()+p.slice(1)).join("");
  const registry=createComponentRegistry();
  for(const tool of tools)for(const side of ["input_schema","output_schema"]){
    const name=pascal(tool.name)+(side==="input_schema"?"Input":"Output");
    registry.component(tool[side],name);
  }
  const projected={...document,components:{...document.components,schemas:{...document.components.schemas,...registry.schemas}}};
  for(const candidate of [document,projected]){
    for(const tool of tools)for(const side of ["input_schema","output_schema"]){
      const name=pascal(tool.name)+(side==="input_schema"?"Input":"Output");
      assertEquivalent(tool[side],candidate,`#/components/schemas/${name}`);
      for(const field of ["$defs","definitions"])for(const definition of Object.keys(tool[side][field]??{})){
        const escaped=definition.replaceAll("~","~0").replaceAll("/","~1");
        resolvePointer(candidate,`#/components/schemas/${name}/${field}/${escaped}`);
      }
    }
    validateLocalRefs(candidate,"generated OpenAPI");
  }
});

test("unchanged generation preserves files and timestamps; check is strict and never writes", () => {
  const directory=mkdtempSync(join(tmpdir(),"opencut-generator-write-"));
  try {
    const path=join(directory,"artifact");
    assert.equal(writeGeneratedFile(path,"first\n"),true);
    const before=statSync(path,{bigint:true});
    assert.equal(writeGeneratedFile(path,"first\n"),false);
    assert.equal(writeGeneratedFile(path,"first\n",true),false);
    const after=statSync(path,{bigint:true});
    assert.equal(after.mtimeNs,before.mtimeNs); assert.equal(after.ino,before.ino);
    assert.throws(()=>writeGeneratedFile(path,"changed\n",true),/stale/);
    assert.equal(readFileSync(path,"utf8"),"first\n");
    assert.throws(()=>writeGeneratedFile(join(directory,"missing"),"x",true),/stale/);
    assert.equal(writeGeneratedFile(path,"changed\n"),true);
  } finally {rmSync(directory,{recursive:true,force:true});}
});
