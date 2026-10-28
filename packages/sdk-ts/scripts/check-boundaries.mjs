import fs from "node:fs";
import path from "node:path";
import { isBuiltin } from "node:module";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = path.resolve(import.meta.dirname,"../../..");
const sourceRoots = ["packages/sdk-ts/src","apps/web/src"];
const bridges = new Map([
  ["apps/web/src/lib/editor-sdk.ts",new Set(["packages/sdk-ts/src/index"])],
  ["apps/web/src/lib/editor-contracts.generated.ts",new Set(["packages/sdk-ts/src/contracts.generated"])],
  // One pure WebAudio graph definition keeps live preview and the reviewed
  // renderer parameters identical; no decoder, host or execution is imported.
  ["apps/web/src/editor/workspace/scene/audio-preview-mastering.ts",new Set(["packages/classic-audio-renderer/src/mastering"])],
  // Native composition owns source decoding/rendering. The sole Web exception
  // decodes already published derived segments; it grants no GPU/host access.
  ["apps/web/src/editor/workspace/scene/visual-preview-renderer.ts",new Set(["packages/classic-visual-renderer/src/preview-segment"])],
]);
// Historical generator lives under src but is a build entry, never a runtime dependency.
const buildOnly = "apps/web/src/editor/generate-contracts.mjs";
const norm = file => file.replaceAll("\\", "/");
const stem = file => file.replace(/\.(?:mjs|cjs|jsx?|tsx?)$/, "");

// Fixtures are test helpers, never runtime dependencies. Use the same precise
// filename classification for traversal and every resolved import target.
export function isTestOnlySource(file) {
  file = norm(file);
  return /\.(?:test|spec)(?:\.[cm]?[jt]sx?)?$/.test(file)
    || /(?:^|\/)(?:fixture|[^/]+-fixture)(?:\.[cm]?[jt]sx?)?$/.test(file);
}

export function violations(file, code, dependencies = new Set()) {
  file = norm(file);
  const owner = sourceRoots.find(r => file.startsWith(r+"/"));
  if (!owner) throw new Error(`Unregistered TypeScript source root: ${file}`);
  const errors=[];
  const check = (specifier, node) => {
    const position = source.getLineAndCharacterOfPosition(node.getStart(source));
    const fail = reason => errors.push(`${file}:${position.line+1}: ${reason}`);
    if (typeof specifier !== "string") return fail("Runtime module lookup must use a static string");
    let resolved;
    if (specifier.startsWith(".")) resolved=norm(path.posix.normalize(path.posix.join(path.posix.dirname(file),specifier)));
    else if (owner === "apps/web/src" && /^(?:@|#)\//.test(specifier)) resolved=path.posix.normalize("apps/web/src/"+specifier.slice(2));
    else {
      const name=specifier.startsWith("@")?specifier.split("/").slice(0,2).join("/"):specifier.split("/")[0];
      if (isBuiltin(specifier) || !dependencies.has(name)) fail(`Undeclared or native runtime dependency: ${specifier}`);
      return;
    }
    if (isTestOnlySource(resolved)) return fail("Runtime source cannot import test-only code");
    if (stem(resolved) === stem(buildOnly)) return fail("Runtime source cannot import the contract build entry");
    if (resolved.startsWith(owner+"/")) return;
    if (bridges.get(file)?.has(stem(resolved))) {
      if (file === "apps/web/src/editor/workspace/scene/visual-preview-renderer.ts") {
        const bindings = ts.isImportDeclaration(node) && node.importClause?.namedBindings;
        if (!bindings || !ts.isNamedImports(bindings) || node.importClause.name ||
            bindings.elements.length !== 1 ||
            (bindings.elements[0].propertyName ?? bindings.elements[0].name).text !== "PreviewSegmentDecoder")
          fail("Native picture adapter may import only the derived segment decoder");
      }
      return;
    }
    fail(`Cross-component implementation import: ${specifier}`);
  };
  const source=ts.createSourceFile(file,code,ts.ScriptTarget.Latest,true);
  for (const diagnostic of source.parseDiagnostics) errors.push(`${file}: ${ts.flattenDiagnosticMessageText(diagnostic.messageText," ")}`);
  const literal = value => value && (ts.isStringLiteralLike(value)) ? value.text : undefined;
  const visit = node => {
    if ((ts.isImportDeclaration(node)||ts.isExportDeclaration(node)) && node.moduleSpecifier) check(literal(node.moduleSpecifier),node);
    if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference)) check(literal(node.moduleReference.expression),node);
    if (ts.isImportTypeNode(node)) check(ts.isLiteralTypeNode(node.argument)?literal(node.argument.literal):undefined,node);
    if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || (ts.isIdentifier(node.expression) && node.expression.text === "require"))) check(literal(node.arguments[0]),node);
    ts.forEachChild(node,visit);
  };
  visit(source);
  return errors;
}

export function inspect() {
  const errors=[]; let files=0;
  for (const sourceRoot of sourceRoots) {
    const packageRoot=path.dirname(sourceRoot);
    const manifest=JSON.parse(fs.readFileSync(path.join(root,packageRoot,"package.json"),"utf8"));
    const dependencies=new Set(Object.keys(manifest.dependencies??{}));
    const configPath=path.join(root,packageRoot,"tsconfig.json");
    if (fs.existsSync(configPath)) {
      const config=ts.readConfigFile(configPath,file=>fs.readFileSync(file,"utf8"));
      if (config.error) throw new Error(`Cannot read ${configPath}`);
      const expected=sourceRoot === "apps/web/src" ? {"#/*":["./src/*"],"@/*":["./src/*"]} : {};
      if (JSON.stringify(config.config.compilerOptions?.paths??{}) !== JSON.stringify(expected)) errors.push(`${packageRoot}: unreviewed TypeScript path aliases`);
    }
    if (manifest.imports && JSON.stringify(manifest.imports)!==JSON.stringify({"#/*":"./src/*"})) errors.push(`${packageRoot}: unreviewed package import aliases`);
    const walk = dir => {
      for (const entry of fs.readdirSync(path.join(root,dir),{withFileTypes:true})) {
        const file=norm(path.posix.join(dir,entry.name));
        if (entry.isSymbolicLink()) {errors.push(`${file}: source symlinks require boundary review`);continue;}
        if (entry.isDirectory()) {walk(file);continue;}
        if (!/\.(?:[cm]?js|jsx|tsx?)$/.test(file) || isTestOnlySource(file) || file===buildOnly) continue;
        files++; errors.push(...violations(file,fs.readFileSync(path.join(root,file),"utf8"),dependencies));
      }
    };
    if (fs.existsSync(path.join(root,sourceRoot))) walk(sourceRoot);
  }
  return {status:errors.length?"failed":"passed",files,errors};
}
if (process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const result=inspect(); console.log(JSON.stringify(result,null,2)); if(result.errors.length) process.exitCode=1;
}
