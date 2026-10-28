import { test } from "node:test";
import assert from "node:assert/strict";
import { violations, inspect } from "./check-boundaries.mjs";
test("allows features to share pure requests and injected state", () =>
  assert.deepEqual(
    violations(
      "editor/timeline/panel.tsx",
      'import {placeAsset} from "../model/edit-requests"; import {useWorkspace} from "../workspace/context";',
    ),
    [],
  ));
test("rejects direct feature coupling", () =>
  assert.equal(
    violations(
      "editor/timeline/panel.tsx",
      'import {upload} from "../assets/upload";',
    ).length,
    1,
  ));
test("rejects reverse state and model dependencies", () => {
  assert.equal(
    violations("editor/model/time.ts", 'import {api} from "../workspace/api";')
      .length,
    1,
  );
  assert.equal(
    violations(
      "editor/workspace/controller.ts",
      'import {Panel} from "../timeline/panel";',
    ).length,
    1,
  );
});
test("rejects a second browser project authority", () =>
  assert.equal(
    violations("editor/projects/store.ts", 'indexedDB.open("projects")').length,
    1,
  ));
test("the retired timeline locator adapter has no storage exception", () => {
  const file = "editor/workspace/scene/timeline-edit/recovery.ts";
  for (const code of ['sessionStorage.getItem("opencut.edit-recovery.v1");', 'sessionStorage.setItem("opencut.edit-recovery.v1",payload);', 'sessionStorage.setItem("opencut.edit-recovery",payload);'])
    assert.equal(violations(file, code).length, 1, code);
});
test("current source obeys boundaries", () =>
  assert.deepEqual(inspect().errors, []));
test("memory test fixtures are excluded from runtime scanning and cannot be runtime dependencies", () => {
  assert.equal(violations("editor/workspace/controller.ts", 'import { setupV4 } from "./scene/fixture";').length, 1);
  assert.equal(violations("editor/workspace/scene/session.ts", 'import { setup } from "./timeline-edit/timeline-fixture";').length, 1);
  assert.deepEqual(violations("editor/workspace/scene/session.test.ts", 'import { setupV4 } from "./fixture";'), []);
});
test("caption control identities have one fixed tab-local adapter, not a general storage exception", () => {
  const file = "editor/workspace/scene/captions/client-state.ts";
  const key = "opencut.caption-client";
  assert.deepEqual(violations(file, `sessionStorage.getItem("${key}"); sessionStorage.setItem("${key}", envelope);`), []);
  for (const code of [
    'sessionStorage.setItem("project", payload);',
    `sessionStorage.removeItem("${key}");`,
    `localStorage.setItem("${key}", payload);`,
    `const storage=sessionStorage; storage.setItem("${key}", payload);`,
    'sessionStorage.setItem(key, payload);',
    'indexedDB.open("captions");',
  ]) assert.equal(violations(file, code).length, 1, code);
  for (const sibling of ["recovery", "submission", "workspace"])
    assert.equal(violations(`editor/workspace/scene/captions/${sibling}.ts`, `sessionStorage.getItem("${key}");`).length, 1);
  assert.deepEqual(violations(file, 'sessionStorage.getItem("opencut.caption-client.v1");'), []);
  assert.ok(violations(file, 'sessionStorage.setItem("opencut.caption-client.v1", envelope);').length > 0);
});
test("edit recovery permits one current locator key and exact original reads", () => {
  const file = "editor/workspace/scene/edit-recovery-pointers.ts";
  assert.deepEqual(violations(file, 'sessionStorage.getItem("opencut.edit-recovery"); sessionStorage.setItem("opencut.edit-recovery",payload); sessionStorage.getItem("opencut.edit-recovery.v2");'), []);
  for (const code of [
    'sessionStorage.setItem("opencut.edit-recovery.v1",payload);',
    'sessionStorage.setItem("opencut.edit-recovery.v2",payload);',
    'localStorage.setItem("opencut.edit-recovery.v2",payload);',
    'const store=sessionStorage; store.setItem("opencut.edit-recovery.v2",payload);',
  ]) assert.equal(violations(file, code).length, 1);
  assert.equal(violations("editor/workspace/controller.ts", 'sessionStorage.setItem("opencut.edit-recovery.v2",payload);').length, 1);
});
test("audio output rejects all browser rendering and mastering imports", () => {
  const path = "../../../../../../packages/classic-audio-renderer/src/mastering.mjs";
  assert.equal(violations("editor/workspace/scene/audio-preview-mastering.ts", `export {createClassicMasteringChain} from "${path}";`).length, 1);
  for (const [file, name] of [
    ["editor/workspace/scene/audio-preview.ts", path],
    ["editor/workspace/scene/audio-preview-mastering.ts", "@opencut/classic-audio-renderer"],
    ["editor/workspace/scene/audio-preview-mastering.ts", "../../../../../../packages/classic-audio-renderer/host/runner.mjs"],
  ]) assert.equal(violations(file, `import x from "${name}";`).length, 1);
});
test("retired recovery paths have no browser storage exception", () => {
  for (const file of ["editor/workspace/classic/source.ts", "editor/projects/reader.ts"])
    for (const code of ['indexedDB.open(name);', 'indexedDB.deleteDatabase(name);', 'localStorage.setItem("project", value);'])
      assert.ok(violations(file, code).length > 0);
});

test("rejects aliases, require and type-only cross-feature imports", () => {
  for (const code of [
    'import {x} from "@/editor/assets/upload";',
    'export * from "#/editor/assets/upload";',
    'const x=require("../assets/upload");',
    'type X=import("../assets/upload").X;',
  ])
    assert.equal(violations("editor/timeline/panel.tsx", code).length, 1);
});
test("rejects runtime require lookup", () =>
  assert.equal(
    violations("editor/timeline/panel.tsx", "const x=require(moduleName);")
      .length,
    1,
  ));
test("UI view preference storage cannot become project or credential storage", () => {
  assert.deepEqual(
    violations(
      "editor/shared/view-preferences.ts",
      'localStorage.getItem("opencut.cover-scenes.v1");',
    ),
    [],
  );
  assert.equal(
    violations(
      "editor/projects/cover.tsx",
      'localStorage.getItem("opencut.cover-scenes.v1");',
    ).length,
    1,
  );
  assert.deepEqual(
    violations(
      "editor/shared/view-preferences.ts",
      'localStorage.setItem("opencut.library", JSON.stringify({layout:"list",sort:"name",ascending:true}));',
    ),
    [],
  );
  assert.deepEqual(
    violations(
      "editor/shared/view-preferences.ts",
      'localStorage.getItem("opencut.timeline");',
    ),
    [],
  );
  assert.deepEqual(
    violations(
      "editor/shared/view-preferences.ts",
      'localStorage.getItem("opencut.history-views.v1");',
    ),
    [],
  );
  assert.equal(
    violations(
      "editor/workspace/controller.ts",
      'localStorage.getItem("opencut.history-views.v1");',
    ).length,
    1,
  );
  for (const code of [
    'localStorage.setItem("token", secret);',
    'localStorage.setItem("opencut.history-views.v1", hints);',
    'localStorage.setItem("opencut.cover-scenes.v1", hints);',
    "const storage=localStorage;",
    "localStorage.getItem(key);",
    'indexedDB.open("projects");',
  ])
    assert.equal(
      violations("editor/shared/view-preferences.ts", code).length,
      1,
    );
});

test("public content and editing authority remain separate", () => {
  assert.ok(
    violations(
      "site/public-pages.tsx",
      'import { WorkspaceController } from "../editor/workspace/controller";',
    ).length,
  );
  assert.ok(
    violations(
      "site/public-data.ts",
      'import { EditorApi } from "../lib/editor-sdk";',
    ).length,
  );
  assert.ok(
    violations(
      "editor/workspace/controller.ts",
      'import { posts } from "../../site/public-data";',
    ).length,
  );
  assert.equal(
    violations(
      "site/site.tsx",
      'import { useLocale } from "../editor/shared/locale";',
    ).length,
    0,
  );
});

test("feedback adapter may persist only its two bounded personal-data keys", () => {
  const file = "editor/feedback/mock-receiver.ts";
  assert.deepEqual(
    violations(
      file,
      'localStorage.getItem("opencut.feedback.draft"); localStorage.setItem("opencut.feedback.mock-history", data); localStorage.getItem("opencut.feedback.mock-history.v1");',
    ),
    [],
  );
  for (const code of [
    'localStorage.setItem("project", data);',
    'localStorage.setItem("opencut.feedback.mock-history.v1", data);',
    'localStorage.setItem("opencut.feedback.draft.v1", data);',
    'localStorage.setItem("token", secret);',
    "localStorage.getItem(key);",
    "localStorage.clear();",
    'localStorage.removeItem("opencut.feedback.draft.v1");',
    "const storage = localStorage;",
    'localStorage["setItem"]("opencut.feedback.draft.v1", data);',
    'indexedDB.open("project");',
  ])
    assert.equal(violations(file, code).length, 1, code);
  assert.equal(
    violations(
      "editor/feedback/dialog.tsx",
      'localStorage.getItem("opencut.feedback.draft.v1");',
    ).length,
    1,
  );
  assert.equal(
    violations(
      "editor/projects/browser.tsx",
      'localStorage.getItem("opencut.feedback.mock-history.v1");',
    ).length,
    1,
  );
  assert.equal(
    violations(
      "editor/feedback/dialog.tsx",
      'import {x} from "../projects/browser";',
    ).length,
    1,
  );
  assert.equal(
    violations(
      "editor/workspace/controller.ts",
      'import {x} from "../feedback/dialog";',
    ).length,
    1,
  );
});
test("native preview permits only the derived segment decoder import", () => {
  const file = "editor/workspace/scene/visual-preview-renderer.ts";
  const decoder = "../../../../../../packages/classic-visual-renderer/src/preview-segment.mjs";
  assert.deepEqual(violations(file, `import {PreviewSegmentDecoder} from "${decoder}";`), []);
  const renderer = "../../../../../../packages/classic-visual-renderer/src/index.mjs";
  for (const code of [
    `import {renderClassicFrame} from "${renderer}";`,
    `import {openGpuRealm, closeGpuRealm, gpuSurfacePresenter} from "${renderer}";`,
    `import * as renderer from "${decoder}";`,
    `export * from "${decoder}";`,
    `import("${decoder}");`,
    `import {PreviewSegmentDecoder, renderClassicFrame} from "${decoder}";`,
    'import x from "../../../../../../packages/classic-visual-renderer/host/runner.mjs";',
  ]) assert.ok(violations(file, code).length > 0, code);
  for (const other of ["editor/workspace/scene/session.ts", "editor/workspace/scene/surface-worker.ts"])
    for (const name of [decoder, renderer])
      assert.ok(violations(other, `import {PreviewSegmentDecoder} from "${name}";`).length > 0);
});

test("shortcut and curve preferences are pinned to exact adapters and literal keys", () => {
  for (const [file,key,historical] of [["editor/shortcuts/persistence.ts","opencut.shortcuts","opencut.shortcuts.v1"],["editor/workspace/scene/keyframes/presets.ts","opencut.curve-presets","opencut.curve-presets.v1"]]) {
    assert.deepEqual(violations(file, `window.localStorage.getItem("${key}"); window.localStorage.setItem("${key}", value); window.localStorage.getItem("${historical}");`), []);
    for (const code of [`localStorage.setItem("${historical}", value);`, `localStorage.setItem("opencut.project", value);`, `localStorage.clear();`, `localStorage.removeItem("${key}");`, `const x = localStorage; x.setItem("${key}", value);`, `localStorage.setItem(key, value);`, `indexedDB.open("anything");`, `sessionStorage.setItem("${key}", value);`])
      assert.ok(violations(file, code).length > 0, code);
    assert.ok(violations("editor/other.ts", `localStorage.setItem("${key}", value);`).length > 0);
  }
});

test("configurable export persists only its bounded fixed identity/options envelope", () => {
  const file="editor/workspace/scene/video-export-recovery.ts", key="opencut.export-video-recovery";
  assert.deepEqual(violations(file, `sessionStorage.getItem("${key}"); sessionStorage.setItem("${key}",envelope);`), []);
  for(const code of [`sessionStorage.getItem(key);`, `const store=sessionStorage; store.getItem("${key}");`, `sessionStorage.clear();`, `sessionStorage.setItem("project",content);`, `localStorage.setItem("${key}",envelope);`]) assert.ok(violations(file,code).length>0,code);
  assert.ok(violations("editor/workspace/scene/video-export.ts",`sessionStorage.setItem("${key}",envelope);`).length>0);
  assert.deepEqual(violations(file, 'sessionStorage.getItem("opencut.export-video-recovery.v2");'), []);
  assert.ok(violations(file, 'sessionStorage.setItem("opencut.export-video-recovery.v2", envelope);').length > 0);
});

test("catalog recovery permits one literal session key only in its reviewed adapter", () => {
  const file = "editor/workspace/scene/catalog_resources-recovery.ts";
  const key = "opencut.catalog-resources";
  assert.deepEqual(violations(file, `sessionStorage.getItem("${key}"); sessionStorage.setItem("${key}", envelope);`), []);
  for (const code of [
    "sessionStorage.getItem(key);",
    'sessionStorage.setItem("opencut.catalog-resources." + project, envelope);',
    'sessionStorage.setItem("opencut.export-video-recovery.v2", envelope);',
    `const storage = sessionStorage; storage.setItem("${key}", envelope);`,
    `const read = sessionStorage.getItem; read("${key}");`,
    `sessionStorage["setItem"]("${key}", envelope);`,
    `window.sessionStorage.setItem("${key}", envelope);`,
    "persistElsewhere(sessionStorage);",
    `sessionStorage.removeItem("${key}");`,
    "sessionStorage.clear();",
    `localStorage.setItem("${key}", envelope);`,
    'indexedDB.open("catalog-project-content");',
  ]) assert.ok(violations(file, code).length > 0, code);
  for (const sibling of [
    "editor/workspace/scene/catalog_resources.ts",
    "editor/workspace/scene/catalog_resources-panel.tsx",
    "editor/workspace/scene/catalog_resources/recovery.ts",
    "editor/workspace/scene/video-export-recovery.ts",
    "editor/workspace/scene/session.ts",
  ]) assert.ok(violations(sibling, `sessionStorage.setItem("${key}", envelope);`).length > 0, sibling);
  assert.deepEqual(violations(file, 'sessionStorage.getItem("opencut.catalog-resources.v1");'), []);
  assert.ok(violations(file, 'sessionStorage.setItem("opencut.catalog-resources.v1", envelope);').length > 0);
});

test("package recovery permits only its bounded current writes and historical reads",()=>{
  const file="editor/workspace/scene/package-recovery.ts",key="opencut.package";
  assert.deepEqual(violations(file,`sessionStorage.getItem("${key}");sessionStorage.setItem("${key}",envelope);`),[]);
  for(const code of [`sessionStorage.getItem(dynamic);`,`sessionStorage.clear();`,`sessionStorage.removeItem("${key}");`,`const store=sessionStorage;store.setItem("${key}",envelope);`,`sessionStorage.setItem("opencut.project",project);`,`localStorage.setItem("${key}",envelope);`])assert.ok(violations(file,code).length>0,code);
  assert.deepEqual(violations(file,'sessionStorage.getItem("opencut.history-package.v1");'),[]);
  assert.ok(violations(file,'sessionStorage.setItem("opencut.history-package.v1",envelope);').length>0);
  assert.ok(violations("editor/workspace/scene/package.tsx",`sessionStorage.setItem("${key}",envelope);`).length>0);
});

test("brand acquisition has one exact session recovery adapter and no shared storage escape",()=>{
  const file="editor/workspace/scene/brand_resources-recovery.ts",key="opencut.brand-resources";
  assert.deepEqual(violations(file,`sessionStorage.getItem("${key}");sessionStorage.setItem("${key}",envelope);`),[]);
  for(const code of [`sessionStorage.getItem(dynamic);`,`sessionStorage.setItem("opencut.brand-resources."+project,envelope);`,`const storage=sessionStorage;storage.setItem("${key}",envelope);`,`window.sessionStorage.setItem("${key}",envelope);`,`sessionStorage["setItem"]("${key}",envelope);`,`sessionStorage.removeItem("${key}");`,`sessionStorage.clear();`,`sessionStorage.setItem("opencut.catalog-resources.v1",envelope);`,`localStorage.setItem("${key}",envelope);`,`indexedDB.open("brands");`])assert.ok(violations(file,code).length>0,code);
  for(const sibling of ["editor/workspace/scene/brand_resources.ts","editor/workspace/scene/brand_resources-panel.tsx","editor/workspace/scene/catalog_resources-recovery.ts","editor/workspace/scene/session.ts"])assert.ok(violations(sibling,`sessionStorage.setItem("${key}",envelope);`).length>0,sibling);
  assert.deepEqual(violations(file, 'sessionStorage.getItem("opencut.brand-resources.v1");'), []);
  assert.ok(violations(file, 'sessionStorage.setItem("opencut.brand-resources.v1", envelope);').length > 0);
});
