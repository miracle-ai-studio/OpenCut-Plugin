import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
const root = path.resolve(import.meta.dirname, "../src");
const features = new Set([
  "assets",
  "effects",
  "feedback",
  "connection",
  "history",
  "inspector",
  "preview",
  "projects",
  "tasks",
  "timeline",
]);
// Match the SDK's reviewed test-source classification. Fixtures may inject
// memory ports, but runtime modules cannot import them through this boundary.
const testSource = (file) => /\.(?:test|spec)(?:\.[cm]?[jt]sx?)?$/.test(file)
  || /(?:^|\/)(?:fixture|[^/]+-fixture)(?:\.[cm]?[jt]sx?)?$/.test(file);
export function violations(file, code) {
  const errors = [],
    source = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, true);
  const owner = file.split("/")[1];
  const check = (name, node) => {
    if (typeof name !== "string") {
      errors.push(`${file}: dynamic module path`);
      return;
    }
    if (
      name.includes("classic-audio-renderer") &&
      !(
        file === "editor/workspace/scene/audio-preview-mastering.ts" &&
        name === "../../../../../../packages/classic-audio-renderer/src/mastering.mjs"
      )
    )
      errors.push(`${file}: renderer imports require the pure mastering bridge`);
    if (name.includes("classic-visual-renderer")) {
      const bindings = node && ts.isImportDeclaration(node) && node.importClause?.namedBindings;
      if (file !== "editor/workspace/scene/visual-preview-renderer.ts" ||
          name !== "../../../../../../packages/classic-visual-renderer/src/preview-segment.mjs" ||
          !bindings || !ts.isNamedImports(bindings) || node.importClause.name ||
          bindings.elements.length !== 1 ||
          (bindings.elements[0].propertyName ?? bindings.elements[0].name).text !== "PreviewSegmentDecoder")
        errors.push(`${file}: visual adapters may import only the derived segment decoder`);
    }
    if (!name.startsWith(".") && !/^(?:@|#)\//.test(name)) return;
    const target = path.posix.normalize(
      /^(?:@|#)\//.test(name)
        ? name.slice(2)
        : path.posix.join(path.posix.dirname(file), name),
    );
    const dependency = target.split("/")[1];
    if (!testSource(file) && testSource(/\.[cm]?[jt]sx?$/.test(target) ? target : `${target}.ts`))
      errors.push(`${file}: runtime imports a test fixture`);
    if (
      file.startsWith("site/") &&
      ((target.startsWith("editor/") && !target.startsWith("editor/shared/")) ||
        target.startsWith("lib/"))
    )
      errors.push(`${file}: public site imports editing authority`);
    if (file.startsWith("editor/") && target.startsWith("site/"))
      errors.push(`${file}: editing module imports public content adapter`);
    if (features.has(owner) && features.has(dependency) && owner !== dependency)
      errors.push(`${file}: feature imports ${dependency}`);
    if (
      ["model", "shared"].includes(owner) &&
      target.startsWith("editor/") &&
      !["model", "shared"].includes(dependency)
    )
      errors.push(`${file}: foundational module imports ${dependency}`);
    if (owner === "workspace" && features.has(dependency))
      errors.push(`${file}: state imports a UI feature`);
  };
  function visit(node) {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteralLike(node.moduleSpecifier)
    )
      check(node.moduleSpecifier.text, node);
    if (
      ts.isCallExpression(node) &&
      (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
        (ts.isIdentifier(node.expression) &&
          node.expression.text === "require"))
    ) {
      const argument = node.arguments[0];
      if (argument && ts.isStringLiteralLike(argument)) check(argument.text);
      else errors.push(`${file}: dynamic module path`);
    }
    if (ts.isImportTypeNode(node))
      check(
        ts.isLiteralTypeNode(node.argument) &&
          ts.isStringLiteralLike(node.argument.literal)
          ? node.argument.literal.text
          : undefined,
      );
    if (
      ts.isImportEqualsDeclaration(node) &&
      ts.isExternalModuleReference(node.moduleReference)
    )
      check(
        ts.isStringLiteralLike(node.moduleReference.expression)
          ? node.moduleReference.expression.text
          : undefined,
      );
    ts.forEachChild(node, visit);
  }
  visit(source);
  const viewPreferences =
    file === "editor/shared/view-preferences.ts" &&
    !/\b(?:indexedDB|EditorCore|sessionStorage)\b/.test(code);
  let storageCalls = 0,
    invalidStorageCall = false;
  function inspectStorage(node) {
    if (ts.isIdentifier(node) && node.text === "localStorage") {
      storageCalls++;
      const property = node.parent,
        call = property?.parent;
      if (
        !ts.isPropertyAccessExpression(property) ||
        !["getItem", "setItem"].includes(property.name.text) ||
        !ts.isCallExpression(call) ||
        !ts.isStringLiteralLike(call.arguments[0]) ||
        ![
          "opencut.library",
          "opencut.timeline",
          "opencut.layout",
          "opencut.welcome",
          "opencut.preview",
          // Finite transaction-scoped selection/scene/playhead hints only.
          // Runtime projection remains in the existing preference module.
          "opencut.history-views",
          // Bounded host/project/scene view identities, no content or image bytes.
          "opencut.cover-scenes",
        ].includes(call.arguments[0].text) && !(property.name.text === "getItem" && ["opencut.history-views.v1", "opencut.cover-scenes.v1"].includes(call.arguments[0].text))
      )
        invalidStorageCall = true;
    }
    ts.forEachChild(node, inspectStorage);
  }
  if (viewPreferences) inspectStorage(source);
  const allowedViews =
    viewPreferences && storageCalls > 0 && !invalidStorageCall;
  // Fixed, bounded UI preferences only. These adapters must not grow into
  // project/media/credential stores. Each exact adapter writes one literal key;
  // its one original preference key remains read-only for existing user data.
  const uiPreferenceKeys = file === "editor/shortcuts/persistence.ts" ? ["opencut.shortcuts", "opencut.shortcuts.v1"]
    : file === "editor/workspace/scene/keyframes/presets.ts" ? ["opencut.curve-presets", "opencut.curve-presets.v1"] : null;
  let uiPreferenceStorage = uiPreferenceKeys !== null && !/\b(?:indexedDB|EditorCore|sessionStorage)\b/.test(code);
  function inspectUiPreferenceStorage(node) {
    if (ts.isIdentifier(node) && node.text === "localStorage") {
      const base = ts.isPropertyAccessExpression(node.parent) && node.parent.name === node ? node.parent : node;
      const property = base.parent, call = property?.parent;
      if (!ts.isPropertyAccessExpression(property) || !["getItem", "setItem"].includes(property.name.text)
          || !ts.isCallExpression(call) || !ts.isStringLiteralLike(call.arguments[0])
          || !(call.arguments[0].text === uiPreferenceKeys[0]
            || (property.name.text === "getItem" && call.arguments[0].text === uiPreferenceKeys[1])))
        uiPreferenceStorage = false;
    }
    ts.forEachChild(node, inspectUiPreferenceStorage);
  }
  if (uiPreferenceStorage) inspectUiPreferenceStorage(source);
  // F57 intentionally keeps only the user's message/draft and 20 local mock
  // receipts. This adapter is not project, media or credential persistence.
  let feedbackStorage =
    file === "editor/feedback/mock-receiver.ts" &&
    !/\b(?:indexedDB|EditorCore|sessionStorage)\b/.test(code);
  function inspectFeedbackStorage(node) {
    if (ts.isIdentifier(node) && node.text === "localStorage") {
      const property = node.parent,
        call = property?.parent;
      if (
        !ts.isPropertyAccessExpression(property) ||
        !["getItem", "setItem"].includes(property.name.text) ||
        !ts.isCallExpression(call) ||
        !ts.isStringLiteralLike(call.arguments[0]) ||
        ![
          "opencut.feedback.draft",
          "opencut.feedback.mock-history",
        ].includes(call.arguments[0].text) && !(property.name.text === "getItem" && ["opencut.feedback.draft.v1", "opencut.feedback.mock-history.v1"].includes(call.arguments[0].text))
      )
        feedbackStorage = false;
    }
    ts.forEachChild(node, inspectFeedbackStorage);
  }
  if (feedbackStorage) inspectFeedbackStorage(source);
  // Recovery adapters persist bounded control identities, not project content.
  // Caption client state additionally retains the exact public ASR submission
  // identity for explicit retry: fixed revision, job key and language only.
  // Its typed parser rejects other fields and bounds the shared envelope to
  // 32 entries / 16 KiB. Subtitle text and edit receipts remain in the host.
  const recoveryStorageKeys = file === "editor/workspace/scene/edit-recovery-pointers.ts" ? ["opencut.edit-recovery", "opencut.edit-recovery.v2"]
    : file === "editor/workspace/scene/captions/client-state.ts" ? ["opencut.caption-client", "opencut.caption-client.v1"]
    : file === "editor/workspace/scene/video-export-recovery.ts" ? ["opencut.export-video-recovery", "opencut.export-video-recovery.v2"]
    : file === "editor/workspace/scene/catalog_resources-recovery.ts" ? ["opencut.catalog-resources", "opencut.catalog-resources.v1"]
    : file === "editor/workspace/scene/brand_resources-recovery.ts" ? ["opencut.brand-resources", "opencut.brand-resources.v1"]
    : file === "editor/workspace/scene/package-recovery.ts" ? ["opencut.package", "opencut.history-package.v1"] : null;
  let recoveryStorage = recoveryStorageKeys !== null &&
    !/\b(?:localStorage|indexedDB|EditorCore)\b/.test(code);
  function inspectRecoveryStorage(node) {
    if (ts.isIdentifier(node) && node.text === "sessionStorage") {
      if (ts.isTypeOfExpression(node.parent)) return;
      const property = node.parent, call = property?.parent;
      if (!ts.isPropertyAccessExpression(property) ||
          !["getItem", "setItem"].includes(property.name.text) ||
          !ts.isCallExpression(call) || !ts.isStringLiteralLike(call.arguments[0]) ||
          !(call.arguments[0].text === recoveryStorageKeys[0] || (property.name.text === "getItem" && call.arguments[0].text === recoveryStorageKeys[1]))) recoveryStorage = false;
    }
    ts.forEachChild(node, inspectRecoveryStorage);
  }
  if (recoveryStorage) inspectRecoveryStorage(source);
  if (
    /\b(?:indexedDB|EditorCore|localStorage|sessionStorage)\b/.test(code) &&
    !(file === "editor/shared/locale.ts" && !/\bsessionStorage\b/.test(code)) &&
    !allowedViews &&
    !uiPreferenceStorage &&
    !feedbackStorage &&
    !recoveryStorage
  )
    errors.push(`${file}: browser-owned project or credential persistence`);
  return errors;
}
export function inspect() {
  const errors = [];
  let files = 0;
  function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (
        /\.tsx?$/.test(file) &&
        !testSource(file)
      ) {
        files++;
        errors.push(
          ...violations(
            path.relative(root, file).replaceAll("\\", "/"),
            fs.readFileSync(file, "utf8"),
          ),
        );
      }
    }
  }
  walk(root);
  return { status: errors.length ? "failed" : "passed", files, errors };
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const result = inspect();
  console.log(JSON.stringify(result, null, 2));
  if (result.errors.length) process.exitCode = 1;
}
