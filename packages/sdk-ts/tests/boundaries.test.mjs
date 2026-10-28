import { test } from "node:test";
import assert from "node:assert/strict";
import { violations, isTestOnlySource, inspect } from "../scripts/check-boundaries.mjs";
const web="apps/web/src/lib/example.ts";
test("TS boundary matches the reviewed derived segment bridge without admitting host or sibling implementations",()=>{
 const bridge="apps/web/src/editor/workspace/scene/visual-preview-renderer.ts";
 const publicPath="../../../../../../packages/classic-visual-renderer/src/preview-segment.mjs";
 assert.deepEqual(violations(bridge,`import {PreviewSegmentDecoder} from "${publicPath}";`),[]);
 for(const target of ["../../../../../../packages/classic-visual-renderer/host/runner.mjs", "../../../../../../packages/classic-visual-renderer/src/decoder.mjs", "../../../../../../packages/classic-audio-renderer/src/index.mjs"]) assert.equal(violations(bridge,`import "${target}";`).length,1);
 assert.equal(violations("apps/web/src/editor/workspace/scene/audio-preview.ts",`import "${publicPath}";`).length,1);
 assert.equal(violations("packages/sdk-ts/src/index.ts",'import "../../classic-visual-renderer/src/index.mjs";').length,1);
});
test("TS boundary accepts local aliases and declared packages",()=>{
 assert.deepEqual(violations(web,'import {x} from "@/editor/x"; export {y} from "./y"; import "react";',new Set(["react"])),[]);
});
test("TS boundary rejects static, re-export, require and import-type escapes",()=>{
 for(const code of ['import {x} from "../../../../crates/native-services/internal";', 'export * from "../../../../packages/sdk-ts/src/transport";', 'const a=require("../../../../apps/api/src/index");','type T=import("../../../../apps/api/src/index").T;']) assert.equal(violations(web,code).length,1);
});
test("TS boundary rejects dynamic lookup and native SDK dependencies",()=>{
 assert.equal(violations(web,'const a=import(target);').length,1);
 assert.equal(violations("packages/sdk-ts/src/a.ts",'import fs from "node:fs";').length,1);
});
test("TS boundary permits only the public SDK bridge target",()=>{
 const bridge="apps/web/src/lib/editor-sdk.ts";
 assert.deepEqual(violations(bridge,'export * from "../../../../packages/sdk-ts/src/index";'),[]);
 assert.equal(violations(bridge,'export * from "../../../../packages/sdk-ts/src/transport";').length,1);
 assert.equal(violations(web,'import "../editor/generate-contracts.mjs";').length,1);
 const audio="apps/web/src/editor/workspace/scene/audio-preview-mastering.ts";
 const pure="../../../../../../packages/classic-audio-renderer/src/mastering.mjs";
 assert.deepEqual(violations(audio,`export {createClassicMasteringChain} from "${pure}";`),[]);
 assert.equal(violations(web,'import "../../../../packages/classic-audio-renderer/src/mastering.mjs";').length,1);
 assert.equal(violations(audio,'import "../../../../../../packages/classic-audio-renderer/src/index.mjs";').length,1);
 assert.equal(violations(audio,'import "../../../../../../packages/classic-audio-renderer/host/runner.mjs";').length,1);
});

test("TS boundary rejects alias traversal and excluded runtime imports",()=>{
 assert.equal(violations(web,'import "@/../../api/src/index";').length,1);
 assert.equal(violations(web,'import "./other.test";').length,1);
 assert.equal(violations(web,'import "./other.spec.ts";').length,1);
 assert.equal(violations(web,'import "../editor/generate-contracts";').length,1);
 assert.equal(violations(web,'import "node:fs";',new Set(["node:fs"])).length,1);
});

test("presentation worker cannot acquire frame execution or native host APIs", () => {
 const file = "apps/web/src/editor/workspace/scene/surface-worker.ts";
 const renderer = "../../../../../../packages/classic-visual-renderer/src/index.mjs";
 assert.ok(violations(file, `import {openGpuRealm, closeGpuRealm, gpuSurfacePresenter} from "${renderer}";`).length > 0);
 for (const code of [
   `import {renderClassicFrame} from "${renderer}";`,
   `import * as renderer from "${renderer}";`,
   `export * from "${renderer}";`,
   `import("${renderer}");`,
   'import x from "../../../../../../packages/classic-visual-renderer/host/runner.mjs";',
 ]) assert.ok(violations(file, code).length > 0, code);
});

test("TS boundary narrowly classifies test helper filenames",()=>{
 for (const file of ["fixture.ts", "editor/media-fixture.ts", "editor/audio-wav-fixture.ts", "editor/a.test.tsx", "editor/a.spec.mjs", "editor/media-fixture"]) assert.equal(isTestOnlySource(file),true,file);
 for (const file of ["editor/fixtures.ts", "editor/fixture-loader.ts", "editor/media-fixture-loader.ts", "editor/fixture/product.ts", "editor/media-fixture/product.ts"]) assert.equal(isTestOnlySource(file),false,file);
});

test("TS boundary rejects runtime references to fixtures through every module form",()=>{
 for (const code of [
  'import {x} from "../editor/workspace/scene/fixture";',
  'export * from "@/editor/workspace/scene/media-fixture.ts";',
  'export {x} from "#/editor/workspace/scene/audio-wav-fixture";',
  'const x=require("../editor/workspace/scene/source-audio-fixture.js");',
  'const x=import("../editor/workspace/scene/media-fixture");',
  'type X=import("../editor/workspace/scene/fixture.ts").X;',
  'import X=require("../editor/workspace/scene/media-fixture");',
 ]) assert.match(violations(web,code).join("\n"),/Runtime source cannot import test-only code/,code);
 assert.match(violations("packages/sdk-ts/src/index.ts",'export * from "./media-fixture";').join("\n"),/test-only code/);
});

test("TS boundary still rejects runtime Vitest and checks current source roots",()=>{
 assert.match(violations(web,'import {vi} from "vitest";',new Set(["react"])).join("\n"),/Undeclared or native runtime dependency/);
 const result=inspect();
 assert.equal(result.status,"passed",result.errors.join("\n"));
 assert.ok(result.files>0);
});
