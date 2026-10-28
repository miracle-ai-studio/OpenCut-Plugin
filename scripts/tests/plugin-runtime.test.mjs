import {test} from 'node:test';
import assert from 'node:assert/strict';
import {workspaceDatabase,dataDirectory} from '../../plugins/opencut/scripts/runtime.mjs';
test('MCP and Web select an explicit absolute database before the default workspace',()=>{
 const previous=process.env.OPENCUT_PLUGIN_DATABASE;
 try {
  delete process.env.OPENCUT_PLUGIN_DATABASE;
  assert.equal(workspaceDatabase(),`${dataDirectory()}/workspace.sqlite`);
  assert.equal(workspaceDatabase({database:'/tmp/selected.sqlite'}),'/tmp/selected.sqlite');
  process.env.OPENCUT_PLUGIN_DATABASE='/tmp/override.sqlite';
  assert.equal(workspaceDatabase({database:'/tmp/selected.sqlite'}),'/tmp/override.sqlite');
  process.env.OPENCUT_PLUGIN_DATABASE='relative.sqlite';assert.throws(()=>workspaceDatabase(),/absolute/);
 }finally{if(previous===undefined)delete process.env.OPENCUT_PLUGIN_DATABASE;else process.env.OPENCUT_PLUGIN_DATABASE=previous;}
});
