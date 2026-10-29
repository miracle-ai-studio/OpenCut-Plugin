import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const pascal = (value) => value.split(/[^a-zA-Z0-9]+/).filter(Boolean)
  .map((part) => part[0].toUpperCase() + part.slice(1)).join("");
const isSchema = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const rootTitle = (schema) => isSchema(schema) && /^[A-Z][A-Za-z0-9]*$/.test(schema.title);

// Resolve against the complete local root, not a definition's name: identical
// definition bodies can have different meanings in different $defs scopes.
export function validateLocalRefs(schema, label = "schema") {
  function visit(value) {
    if (!value || typeof value !== "object") return;
    if (Object.hasOwn(value, "$ref")) {
      const ref = value.$ref;
      if (typeof ref !== "string" || (ref !== "#" && !ref.startsWith("#/"))) {
        throw new Error(`Unsupported schema ref in ${label}: ${ref}`);
      }
      let target = schema;
      for (const token of ref === "#" ? [] : ref.slice(2).split("/")) {
        if (/~(?![01])/.test(token)) throw new Error(`Invalid schema pointer in ${label}: ${ref}`);
        const key = token.replaceAll("~1", "/").replaceAll("~0", "~");
        if (!target || typeof target !== "object" || !Object.hasOwn(target, key)) {
          throw new Error(`Unresolved schema ref in ${label}: ${ref}`);
        }
        target = target[key];
      }
      if (typeof target !== "boolean" && !isSchema(target)) {
        throw new Error(`Ref does not select a schema in ${label}: ${ref}`);
      }
    }
    for (const child of Object.values(value)) visit(child);
  }
  visit(schema);
}

export function preflightRegistry({ tools, jobs, domains, eventSchema, audioStream, visualStream, uploads }) {
  if (!Array.isArray(tools) || !Array.isArray(jobs) || !Array.isArray(domains)) {
    throw new Error("Invalid registry arrays");
  }
  const names = new Set(), namespaces = new Set();
  function namespace(name) {
    if (namespaces.has(name)) throw new Error(`Colliding generated namespace: ${name}`);
    namespaces.add(name);
  }
  function schema(value, label) {
    if (!isSchema(value)) throw new Error(`Missing schema: ${label}`);
    validateLocalRefs(value, label);
  }
  for (const tool of tools) {
    if (!tool || !/^opencut_[a-z0-9_]+$/.test(tool.name) || names.has(tool.name)) {
      throw new Error(`Invalid or duplicate tool: ${tool?.name}`);
    }
    names.add(tool.name);
    for (const side of ["input_schema", "output_schema"]) {
      schema(tool[side], `${tool.name}.${side}`);
      namespace(`${pascal(tool.name)}${side === "input_schema" ? "Input" : "Output"}`);
    }
    if (!rootTitle(tool.input_schema) || tool.output_schema.title !== "Response") {
      throw new Error(`Unsupported root type in ${tool.name}`);
    }
  }
  const jobNames = new Set();
  for (const job of jobs) {
    if (!job || typeof job.name !== "string" || !job.name || jobNames.has(job.name)) {
      throw new Error(`Invalid or duplicate job: ${job?.name}`);
    }
    jobNames.add(job.name);
    for (const side of ["result_schema"]) {
      schema(job[side], `${job.name}.${side}`);
      namespace(`Job${pascal(job.name)}${side === "input_schema" ? "Input" : "Result"}`);
    }
    if (!rootTitle(job.result_schema)) throw new Error(`Unsupported result type in ${job.name}`);
  }
  const domainNames = new Set(), mapped = new Set();
  for (const domain of domains) {
    if (!domain || !/^[a-z][a-z0-9_]*$/.test(domain.domain)
        || domainNames.has(domain.domain) || !Array.isArray(domain.actions)) {
      throw new Error("Invalid or duplicate domain descriptor");
    }
    domainNames.add(domain.domain);
    const actions = new Set();
    for (const entry of domain.actions) {
      if (!entry || typeof entry.action !== "string" || !/^[a-z][a-z0-9.]*$/.test(entry.action)
          || actions.has(entry.action) || !names.has(entry.operation) || mapped.has(entry.operation)) {
        throw new Error(`Invalid or duplicate domain action in ${domain.domain}`);
      }
      actions.add(entry.action);
      mapped.add(entry.operation);
    }
  }
  if (mapped.size !== names.size) throw new Error("Domain registry does not cover every operation");
  schema(eventSchema, "event stream");
  for (const [name, value] of [["AudioPreviewTransport", audioStream?.input], ["AudioPreviewPcm", audioStream?.header]]) {
    namespace(name);
    schema(value, name);
    if (!rootTitle(value)) throw new Error(`Unsupported root type in ${name}`);
  }
  {
    for (const [name, value] of [["VisualPreviewTransport", visualStream?.input], ["VisualPreviewPixels", visualStream?.descriptor]]) {
      namespace(name); schema(value, name);
      if (!rootTitle(value)) throw new Error(`Unsupported root type in ${name}`);
    }
    if (visualStream.content_type !== "application/vnd.opencut.rgba" || visualStream.pixel_format !== "rgba8" || visualStream.chunk_bytes !== 65536)
      throw new Error("Unsupported native picture byte transport");
    const prepare = visualStream.prepare;
    for (const [name, value] of [["VisualPreviewPreparation", prepare?.input], ["VisualPreviewPrepared", prepare?.header]]) {
      namespace(name); schema(value, name);
      if (!rootTitle(value)) throw new Error(`Unsupported root type in ${name}`);
    }
    if (prepare.path !== "/api/preview/picture/prepare" || prepare.content_type !== "application/vnd.opencut.picture" ||
        prepare.magic !== "OCVP" || prepare.prefix_bytes !== 12 || prepare.max_header_bytes !== 2097152 || prepare.chunk_bytes !== 65536)
      throw new Error("Unsupported prepared native picture transport");
  }
  if (uploads && (uploads.chunk_method !== "PUT" || uploads.chunk_path_template !== "/api/uploads/{upload_id}/chunks/{offset}"
      || !Array.isArray(uploads.chunk_query_fields) || uploads.chunk_query_fields.join(",") !== "size_bytes,content_hash")) {
    throw new Error("Unsupported upload byte transport in host capabilities");
  }
}

export function memoizeSchemaCompiler(compile) {
  const cache = new Map();
  return (schema, title, options) => {
    // Preserve property/enum order too: compiler output bytes can depend on it.
    const key = JSON.stringify([schema, title, options]);
    if (!cache.has(key)) cache.set(key, Promise.resolve().then(() => compile(schema, title, options)));
    return cache.get(key);
  };
}

export function createComponentRegistry() {
  const schemas = {}, roots = new Map(), definitions = new Map();
  function hasScopedIdentity(value) {
    if (!value || typeof value !== "object") return false;
    return ["$id", "$anchor", "$dynamicAnchor", "$dynamicRef", "$recursiveAnchor", "$recursiveRef"]
      .some((key) => Object.hasOwn(value, key)) || Object.values(value).some(hasScopedIdentity);
  }
  const pointerToken = (key) => key.replaceAll("~", "~0").replaceAll("/", "~1");
  function references(value, found = new Set()) {
    if (!value || typeof value !== "object") return found;
    if (Object.hasOwn(value, "$ref")) found.add(value.$ref);
    for (const child of Object.values(value)) references(child, found);
    return found;
  }
  function hasDefinitionContainer(value) {
    if (!value || typeof value !== "object") return false;
    return Object.hasOwn(value, "$defs") || Object.hasOwn(value, "definitions")
      || Object.values(value).some(hasDefinitionContainer);
  }
  function definitionKey(schema, body) {
    const targets = new Map(), pending = [...references(body)];
    while (pending.length) {
      const ref = pending.pop();
      if (targets.has(ref)) continue;
      let target = schema;
      for (const key of ref === "#" ? [] : ref.slice(2).split("/")) {
        target = target[key.replaceAll("~1", "/").replaceAll("~0", "~")];
      }
      targets.set(ref, target);
      pending.push(...references(target));
    }
    // Ref names are part of the complete context, including root-dependent
    // targets. Visited pointers terminate cycles without deleting semantics.
    return JSON.stringify([body, [...targets].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)]);
  }
  function component(schema, name) {
    if (Object.hasOwn(schemas, name)) throw new Error(`Duplicate OpenAPI component: ${name}`);
    validateLocalRefs(schema, name);
    const key = JSON.stringify(schema);
    // Base IDs and dynamic scopes are not closed local roots; retain their
    // existing representation rather than changing resolution through aliases.
    const shareable = !hasScopedIdentity(schema);
    const refs = [...references(schema)];
    const nestedEntries = refs.some((ref) => /^#\/(?:\$defs|definitions)\/[^/]+\//.test(ref))
      || ["$defs", "definitions"].some((field) => Object.values(schema[field] ?? {}).some(hasDefinitionContainer));
    const existing = shareable && !nestedEntries ? roots.get(key) : undefined;
    // Share only an entire closed root. Keep every operation's component name.
    if (existing) {
      schemas[name] = { $ref: `#/components/schemas/${existing}` };
      // Preserve old definition-entry links as well as the public root name.
      for (const field of ["$defs", "definitions"]) {
        if (schema[field]) schemas[name][field] = Object.fromEntries(Object.keys(schema[field]).map((key) =>
          [key, { $ref: `#/components/schemas/${existing}/${field}/${pointerToken(key)}` }]));
      }
    }
    else {
      function rebase(value) {
        if (Array.isArray(value)) return value.map(rebase);
        if (!value || typeof value !== "object") return value;
        return Object.fromEntries(Object.entries(value).map(([key, val]) =>
          [key, key === "$ref" ? `#/components/schemas/${name}${val.slice(1)}` : rebase(val)]));
      }
      schemas[name] = rebase(schema);
      if (shareable) {
        if (!nestedEntries) roots.set(key, name);
        for (const field of ["$defs", "definitions"]) {
          for (const [definition, body] of Object.entries(schema[field] ?? {})) {
            const pointer = `#/${field}/${pointerToken(definition)}`;
            // An alias would erase physical descendant-pointer entries. Keep
            // their parent definition intact; ordinary named entries still share.
            if (hasDefinitionContainer(body) || refs.some((ref) => ref.startsWith(pointer + "/"))) continue;
            const closure = definitionKey(schema, body);
            const canonical = definitions.get(closure);
            if (canonical) schemas[name][field][definition] = { $ref: canonical };
            else definitions.set(closure, `#/components/schemas/${name}${pointer.slice(1)}`);
          }
        }
      }
    }
    return { $ref: `#/components/schemas/${name}` };
  }
  return { schemas, component };
}

export function writeGeneratedFile(path, content, check = false) {
  let existing;
  try { existing = readFileSync(path, "utf8"); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  if (existing === content) return false;
  if (check) throw new Error(`Generated contracts are stale: ${path}. Run npm run contracts:generate in packages/sdk-ts.`);
  writeFileSync(path, content);
  return true;
}

export async function generateContracts(compile, args = process.argv.slice(2)) {
const root = resolve(import.meta.dirname, "../../..");
const target = resolve(root, "packages/sdk-ts/src/contracts.generated.ts");
const argumentsSet = new Set(args);
const binaryIndex = args.indexOf("--binary");
const suppliedBinary = binaryIndex >= 0 ? args[binaryIndex + 1] : undefined;
if (binaryIndex >= 0 && (!suppliedBinary || suppliedBinary.startsWith("--"))) {
  throw new Error("--binary requires a path");
}
const binary = suppliedBinary ? resolve(suppliedBinary) : resolve(
  root,
  "target/debug",
  process.platform === "win32" ? "opencut.exe" : "opencut",
);
if (binaryIndex >= 0) {
  argumentsSet.delete("--binary");
  argumentsSet.delete(suppliedBinary);
}
const supportedArguments = new Set(["--check", "--skip-build"]);
const unknownArguments = [...argumentsSet].filter(
  (argument) => !supportedArguments.has(argument),
);
if (unknownArguments.length > 0) {
  throw new Error(`Unknown argument(s): ${unknownArguments.join(", ")}`);
}
const check = argumentsSet.has("--check");
const skipBuild = argumentsSet.has("--skip-build");

if (!skipBuild && !suppliedBinary) {
  execFileSync("cargo", ["build", "-p", "opencut-cli"], {
    cwd: root,
    stdio: "inherit",
  });
}

// Versioned complete aggregate schemas exceed Node's default 1 MiB pipe
// buffer. Keep registry capture bounded; media payloads never enter it.
const registryOptions = { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 };
const envelope = JSON.parse(
  execFileSync(binary, ["system", "schema", "--json"], registryOptions),
);
if (envelope.error || !Array.isArray(envelope.data))
  throw new Error(envelope.error?.message ?? "Invalid schema registry");
const capabilitiesEnvelope = JSON.parse(
  execFileSync(binary, ["system", "capabilities", "--json"], registryOptions),
);
const domainsEnvelope = JSON.parse(
  execFileSync(binary, ["system", "domains", "--json"], registryOptions),
);
const eventSchemaEnvelope = JSON.parse(
  execFileSync(binary, ["system", "events-schema", "--json"], registryOptions),
);
if (domainsEnvelope.error || !Array.isArray(domainsEnvelope.data)) {
  throw new Error(domainsEnvelope.error?.message ?? "Invalid domain registry");
}
if (
  capabilitiesEnvelope.error ||
  !capabilitiesEnvelope.data ||
  !Array.isArray(capabilitiesEnvelope.data.job_results)
) {
  throw new Error(
    capabilitiesEnvelope.error?.message ?? "Invalid job schema registry",
  );
}
if (eventSchemaEnvelope.error || !eventSchemaEnvelope.data ||
    typeof eventSchemaEnvelope.data !== "object" || Array.isArray(eventSchemaEnvelope.data)) {
  throw new Error(eventSchemaEnvelope.error?.message ?? "Invalid event stream schema");
}
const audioStreamEnvelope = JSON.parse(execFileSync(binary,
  ["system", "audio-preview-schema", "--json"], registryOptions));
if (audioStreamEnvelope.error || !audioStreamEnvelope.data ||
    !audioStreamEnvelope.data.input || !audioStreamEnvelope.data.header) {
  throw new Error("Invalid V4 audio stream schema registry");
}
const visualStreamEnvelope = JSON.parse(execFileSync(binary,
  ["system", "visual-preview-schema", "--json"], registryOptions));
if (visualStreamEnvelope.error || !visualStreamEnvelope.data?.input || !visualStreamEnvelope.data?.descriptor)
  throw new Error("Invalid native picture stream schema registry");
preflightRegistry({ tools: envelope.data, jobs: capabilitiesEnvelope.data.job_results,
  domains: domainsEnvelope.data, eventSchema: eventSchemaEnvelope.data,
  audioStream: audioStreamEnvelope.data, visualStream: visualStreamEnvelope.data, uploads: capabilitiesEnvelope.data.uploads });
compile = memoizeSchemaCompiler(compile);
let output =
  "// Generated from the Rust automation registry. Do not edit.\n\n";
const toolNames = new Set(envelope.data.map((tool) => tool.name));
for (const tool of envelope.data) {
  for (const side of ["input_schema", "output_schema"]) {
    const namespace = `${pascal(tool.name)}${side === "input_schema" ? "Input" : "Output"}`;
    const source = await compile(tool[side], "Schema", {
      bannerComment: "",
      style: { singleQuote: true },
      unknownAny: false,
    });
    output += `export namespace ${namespace} {\n${source.replace(/^export /gm, "  export ")}\n}\n\n`;
  }
}
for (const job of capabilitiesEnvelope.data.job_results) {
  for (const side of ["result_schema"]) {
    const namespace = `Job${pascal(job.name)}${side === "input_schema" ? "Input" : "Result"}`;
    const source = await compile(job[side], "Schema", {
      bannerComment: "",
      style: { singleQuote: true },
      unknownAny: false,
    });
    output += `export namespace ${namespace} {\n${source.replace(/^export /gm, "  export ")}\n}\n\n`;
  }
}

for (const [namespace, schema] of [
  ["AudioPreviewTransport", audioStreamEnvelope.data.input],
  ["AudioPreviewPcm", audioStreamEnvelope.data.header],
  ["VisualPreviewTransport", visualStreamEnvelope.data.input],
  ["VisualPreviewPixels", visualStreamEnvelope.data.descriptor],
  ["VisualPreviewPreparation", visualStreamEnvelope.data.prepare.input],
  ["VisualPreviewPrepared", visualStreamEnvelope.data.prepare.header],
]) {
  const source = await compile(schema, "Schema", {
    bannerComment: "", style: { singleQuote: true }, unknownAny: false,
  });
  output += `export namespace ${namespace} {\n${source.replace(/^export /gm, "  export ")}\n}\n\n`;
}
const { input: _prepareInput, header: _prepareHeader, ...picturePreparationTransport } = visualStreamEnvelope.data.prepare;
output += `export const picturePreparationTransport = ${JSON.stringify(picturePreparationTransport, null, 2)} as const;\n\n`;
output += "export interface ToolMap {\n";
for (const tool of envelope.data) {
  const title = tool.input_schema.title;
  output += `  ${tool.name}: { input: ${pascal(tool.name)}Input.${title}; output: NonNullable<${pascal(tool.name)}Output.Response["data"]> };\n`;
}
output += "}\n";
output += "export interface DomainMap {\n";
for (const domain of domainsEnvelope.data) {
  output += `  ${JSON.stringify(domain.domain)}: {\n`;
  for (const entry of domain.actions) {
    output += `    ${JSON.stringify(entry.action)}: ToolMap[${JSON.stringify(entry.operation)}];\n`;
  }
  output += "  };\n";
}
output += "}\n";
output += "export const domainNames = " + JSON.stringify(domainsEnvelope.data.map(d => d.domain), null, 2) + " as const;\n";
output += "export const domainActions = " + JSON.stringify(Object.fromEntries(domainsEnvelope.data.map(d => [d.domain, d.actions.map(a => a.action)])), null, 2) + " as const;\n";
output += "export interface JobResultMap {\n";
for (const job of capabilitiesEnvelope.data.job_results) {
  const title = job.result_schema.title;
  output += `  "${job.name}": Job${pascal(job.name)}Result.${title};\n`;
}
output += "}\n";

output += "export const toolNames = " + JSON.stringify([...toolNames], null, 2) + " as const;\n";

// JSON Schema local refs must be rebased when embedded into an OpenAPI document.
const { schemas, component } = createComponentRegistry();
const paths = {};
for (const tool of envelope.data) {
  const input = component(tool.input_schema, `${pascal(tool.name)}Input`);
  const output = component(tool.output_schema, `${pascal(tool.name)}Output`);
  const response = { description: "OpenCut response envelope; inspect error.code on failure.", content: { "application/json": { schema: output } } };
  paths[`/api/tools/${tool.name}`] = { post: {
    operationId: tool.name,
    description: tool.description,
    "x-opencut-read-only": tool.read_only,
    requestBody: { required: true, content: { "application/json": { schema: input } } },
    responses: { "200": response, default: response },
  } };
}
for (const domain of domainsEnvelope.data) {
  const variants = domain.actions.map((action) => ({
    type: "object",
    properties: {
      action: { const: action.action },
      input: { $ref: `#/components/schemas/${pascal(action.operation)}Input` },
    },
    required: ["action", "input"],
    additionalProperties: false,
  }));
  const resultVariants = domain.actions.map((action) => ({
    $ref: `#/components/schemas/${pascal(action.operation)}Output`,
  }));
  paths[`/api/tools/opencut_${domain.domain}`] = { post: {
    operationId: `opencut_${domain.domain}`,
    description: `${domain.description} The action selects the exact legacy operation and its input schema.`,
    "x-opencut-action-operations": Object.fromEntries(domain.actions.map(a => [a.action, a.operation])),
    requestBody: { required: true, content: { "application/json": { schema: { oneOf: variants } } } },
    responses: {
      "200": { description: "Response envelope for the selected action.", content: { "application/json": { schema: { anyOf: resultVariants } } } },
      default: { description: "Structured OpenCut error response.", content: { "application/json": { schema: { type: "object" } } } },
    },
  } };
}
schemas.DomainActionDescriptor = {
  type: "object", required: ["action", "operation", "description", "read_only"],
  properties: {
    action: { type: "string" }, operation: { type: "string" },
    description: { type: "string" }, read_only: { type: "boolean" },
    input_schema: { type: "object" }, output_schema: { type: "object" },
  }, additionalProperties: false,
};
schemas.DomainDescriptor = {
  type: "object", required: ["domain", "description", "actions"],
  properties: {
    domain: { type: "string" }, description: { type: "string" },
    actions: { type: "array", items: { $ref: "#/components/schemas/DomainActionDescriptor" } },
  }, additionalProperties: false,
};
schemas.DomainCatalogEnvelope = {
  type: "object", required: ["request_id", "data", "error", "warnings"],
  properties: {
    request_id: { type: "string" },
    data: { anyOf: [{ type: "array", items: { $ref: "#/components/schemas/DomainDescriptor" } }, { type: "null" }] },
    error: { anyOf: [{ type: "object" }, { type: "null" }] },
    warnings: { type: "array", items: { type: "string" } },
  }, additionalProperties: false,
};
const domainResponse = {
  description: "Authenticated domain catalog or exact per-action schema envelope.",
  content: { "application/json": { schema: { $ref: "#/components/schemas/DomainCatalogEnvelope" } } },
};
paths["/api/domains"] = { get: {
  operationId: "opencut_domains_list", description: "Discover domains and action summaries without expanding per-action schemas.",
  responses: { "200": domainResponse, default: domainResponse },
} };
paths["/api/domains/{domain}"] = { get: {
  operationId: "opencut_domain_inspect", description: "Get one domain with exact input/output schemas for every action.",
  parameters: [{ name: "domain", in: "path", required: true, schema: { type: "string", enum: domainsEnvelope.data.map(d => d.domain) } }],
  responses: { "200": domainResponse, default: domainResponse },
} };
schemas.OpenCutErrorEnvelope = {
  type: "object", required: ["request_id", "data", "error", "warnings"],
  properties: {
    request_id: { type: "string" }, data: { type: "null" },
    error: { type: "object", required: ["code", "message", "details"],
      properties: { code: { type: "string" }, message: { type: "string" }, details: true },
      additionalProperties: false },
    warnings: { type: "array", items: { type: "string" } },
  }, additionalProperties: false,
};
const transportError = {
  description: "Structured OpenCut error envelope; inspect error.code before retrying.",
  content: { "application/json": { schema: { $ref: "#/components/schemas/OpenCutErrorEnvelope" } } },
};
const audioStreamInput = component(audioStreamEnvelope.data.input, "AudioPreviewTransport");
component(audioStreamEnvelope.data.header, "AudioPreviewPcmHeaderV4");
paths["/api/preview/audio/stream"] = { post: {
  operationId: "opencut_audio_preview_stream",
  description: "Bounded fixed-generation mastered stereo Float32. Each OCAP frame has 12-byte little-endian lengths, a generated JSON header with pcm_stage=mastered, and a maximum 32768-byte interleaved payload. Consumers present completed PCM directly without another compressor, gain or look-ahead delay. Actual consumption is acknowledged through the control operation.",
  requestBody: { required: true, content: { "application/json": { schema: audioStreamInput } } },
  responses: { "200": { description: "OCAP framed mastered PCM; maximum 131072 unconsumed frames.", content: {
    "application/vnd.opencut.audio-preview": { schema: { type: "string", format: "binary" } },
  } }, default: transportError },
} };
const nativePictureInput = component(visualStreamEnvelope.data.input, "VisualPreviewPictureRead");
paths["/api/preview/picture/stream"] = { post: {
  operationId: "opencut_visual_preview_picture_stream",
  description: "Claim a single-use native RGBA picture descriptor from the complete scene frame operation. Current owner, lease, generation and authority are checked for every bounded 64 KiB chunk. Active readers remain in the captured residency budget.",
  requestBody: { required: true, content: { "application/json": { schema: nativePictureInput } } },
  responses: { "200": { description: "Exact straight RGBA8 bytes; dimensions, length and SHA-256 are supplied by the generated picture descriptor.", content: {
    "application/vnd.opencut.rgba": { schema: { type: "string", format: "binary" } },
  } }, default: transportError },
} };
const picturePrepare = visualStreamEnvelope.data.prepare;
const picturePrepareInput = component(picturePrepare.input, "VisualPreviewPicturePrepare");
component(picturePrepare.header, "VisualPreviewPreparedPicture");
paths[picturePrepare.path] = { post: {
  operationId: "opencut_visual_preview_picture_prepare",
  description: "Execute the same scene or edit-draft frame use case and claim its pixels before responding. OCVP contains12 little-endian prefix bytes (magic,JSON-header length,RGBA length),a generated prepared-picture JSON header bounded to2MiB,and exact straight RGBA8 bytes. Metadata and pixels retain64KiB live authorization/generation checkpoints,one-second backpressure and the original residency budget. Receiving metadata is not presentation;verify the full pixel length/hash before use.",
  requestBody: { required: true, content: { "application/json": { schema: picturePrepareInput } } },
  responses: { "200": { description: "Already-claimed native picture with generated metadata and bounded RGBA payload.", content: {
    [picturePrepare.content_type]: { schema: { type: "string", format: "binary" } },
  } }, default: transportError },
} };
const previewFontTool = envelope.data.find(tool => tool.name === "opencut_visual_preview_font_authorize");
if (previewFontTool) {
  paths["/api/preview/visual-font/stream"] = { post: {
    operationId: "opencut_visual_preview_font_stream",
    description: "Read the participating locked font under the live preview owner, lease and generation. Authorization is rechecked while streaming; cached font reuse requires the same authorization operation.",
    requestBody: paths["/api/tools/opencut_visual_preview_font_authorize"].post.requestBody,
    responses: { "200": { description: "Verified font bytes with no-store caching.", content: {
      "application/octet-stream": { schema: { type: "string", format: "binary" } },
    } }, default: transportError },
  } };
}
const uploads = capabilitiesEnvelope.data.uploads;
if (uploads) {
  paths[uploads.chunk_path_template] = { put: {
    operationId: "opencut_upload_chunk",
    description: `Send sequential raw bytes with the chunk SHA-256 and canonical decimal offset/size. The full-file digest is required only by opencut_upload_complete. Maximum ${uploads.max_chunk_bytes} bytes per chunk; source offsets are exactly addressable through ${uploads.max_source_bytes} bytes and actual writes use configured storage admission. At most ${uploads.max_inflight_chunks} concurrent requests and ${uploads.chunk_timeout_seconds} seconds receiving. A lost response has an unknown outcome: inspect the confirmed next_offset before retrying identical bytes. Project read, edit and run_jobs permissions are required.`,
    parameters: [
      { name: "upload_id", in: "path", required: true, schema: { type: "string", pattern: "^upload_[a-f0-9]{32}$" } },
      { name: "offset", in: "path", required: true, schema: { type: "string", pattern: "^(0|[1-9][0-9]*)$" } },
      { name: "size_bytes", in: "query", required: true, schema: { type: "string", pattern: "^[1-9][0-9]*$" } },
      { name: "content_hash", in: "query", required: true, schema: { type: "string", pattern: "^[a-f0-9]{64}$" } },
    ],
    requestBody: { required: true, content: { "application/octet-stream": { schema: { type: "string", format: "binary" } } } },
    responses: {
      "200": { description: "Confirmed upload state and next_offset.", content: { "application/json": { schema: { $ref: "#/components/schemas/OpencutUploadInspectOutput" } } } },
      default: transportError,
    },
  } };

}
paths["/api/projects/{project}/media/{media_id}/content"] = { get: {
  operationId: "opencut_project_media_content",
  description: "Read the verified fixed bytes of an imported project media resource. Requires the current project's read grant; the opaque media ID does not grant access by itself. Long responses revalidate authorization after policy changes. The response is not a host path or an owner-only global media URL.",
  "x-opencut-read-only": true,
  parameters: [
    { name: "project", in: "path", required: true, schema: { type: "string" } },
    { name: "media_id", in: "path", required: true, schema: { type: "string", pattern: "^media_[a-f0-9]{32}$" } },
  ],
  responses: {
    "200": { description: "Verified media bytes with detected Content-Type and Content-Length.",
      content: { "application/octet-stream": { schema: { type: "string", format: "binary" } } } },
    default: transportError,
  },
} };
paths["/api/project-resources/content"] = { get: {
  operationId: "opencut_project_resource_content",
  description: "Read exact verified asset/font bytes selected from a fixed project revision under live project authorization. Query identifiers never become path segments. Current credentials, grant and project presence are checked before bytes and during streaming.",
  "x-opencut-read-only": true,
  parameters: [
    { name: "project_id", in: "query", required: true, schema: { type: "string" } },
    { name: "revision", in: "query", required: true, schema: { type: "string" } },
    { name: "kind", in: "query", required: true, schema: { type: "string", enum: ["asset", "font"] } },
    { name: "resource_id", in: "query", required: true, schema: { type: "string" } },
    { name: "Range", in: "header", schema: { type: "string" } },
  ],
  responses: {
    "200": { description: "Verified bytes.", content: { "application/octet-stream": { schema: { type: "string", format: "binary" } } } },
    "206": { description: "Verified byte range; Content-Range describes the served span." },
    "416": { description: "Invalid or unsatisfiable byte range." },
    default: transportError,
  },
} };
paths["/api/artifacts/{artifact}"] = { get: {
  operationId: "opencut_artifact_download",
  description: "Download the bytes of a published job artifact identified by job ID and SHA-256. Authorization and integrity are checked against the committed job; a historical successful job does not guarantee that its file remains available. Content-Type identifies the actual media format.",
  parameters: [{ name: "artifact", in: "path", required: true,
    schema: { type: "string", pattern: "^artifact:job_[a-f0-9]{32}:[a-f0-9]{64}$" } }],
  responses: {
    "200": { description: "Verified artifact bytes, with actual Content-Type and Content-Length headers.",
      content: { "application/octet-stream": { schema: { type: "string", format: "binary" } } } },
    default: transportError,
  },
} };
paths["/api/changes/stream"] = { post: {
  operationId: "opencut_changes_stream",
  description: "Observe public project or job changes with an opaque cursor. SSE data contains one opencut.events.v1 record; the SSE id is its resume cursor. Last-Event-ID overrides the body cursor. Reconnect using the last fully processed id. Closing observation does not cancel durable tasks.",
  "x-opencut-read-only": true,
  parameters: [{ name: "Last-Event-ID", in: "header", required: false, schema: { type: "string" } }],
  requestBody: { required: true, content: { "application/json": { schema: { $ref: "#/components/schemas/OpencutEventChangesInput" } } } },
  responses: {
    "200": { description: "SSE ready/change/error records; each data field is JSON matching x-opencut-data-schema.",
      content: { "text/event-stream": { schema: { type: "string" },
        "x-opencut-data-schema": component(eventSchemaEnvelope.data, "OpenCutEventStreamRecord") } } },
    default: transportError,
  },
} };
paths["/api/preview/stream"] = { post: {
  operationId: "opencut_preview_stream",
  description: "Claim the current owner generation's single bounded fMP4 stream. Requires bearer authorization plus owner lease. Disconnect cancels that generation. Host restart ends all preview sessions.",
  requestBody: { required: true, content: { "application/json": { schema: {
    type: "object", additionalProperties: false,
    required: ["session_id", "generation", "owner_id", "lease_id"],
    properties: Object.fromEntries(["session_id", "generation", "owner_id", "lease_id"].map(key => [key, { type: "string" }]))
  } } } },
  responses: {
    "200": { description: "One H.264/AAC fragmented MP4 generation. Encoding progress does not establish client presentation.",
      content: { "video/mp4": { schema: { type: "string", format: "binary" } } } },
    default: transportError,
  },
} };
const openapi = {
  openapi: "3.1.0",
  jsonSchemaDialect: "https://json-schema.org/draft/2020-12/schema",
  info: { title: "OpenCut automation tool API", version: "1.0.0", description: "Domain and legacy automation endpoints plus raw project-upload chunks, scoped project media reads, published artifact bytes and public event SSE. Legacy v1 job paths are host-local and are not remote URLs." },
  security: [{ bearerAuth: [] }], paths,
  components: { securitySchemes: { bearerAuth: { type: "http", scheme: "bearer" } }, schemas },
};
const files = new Map([
  [target, output],
  [resolve(root, "packages/contract-fixtures/tools.generated.json"), JSON.stringify(envelope.data, null, 2) + "\n"],
  [resolve(root, "packages/contract-fixtures/openapi.generated.json"), JSON.stringify(openapi, null, 2) + "\n"],
  [resolve(root, "packages/contract-fixtures/event-stream.generated.json"), JSON.stringify(eventSchemaEnvelope.data, null, 2) + "\n"],
  [resolve(root, "apps/web/src/lib/editor-contracts.generated.ts"), '// Compatibility re-export; authoritative types live in the standalone SDK.\nexport * from "../../../../packages/sdk-ts/src/contracts.generated";\n'],
]);
for (const [path, content] of files) {
  writeGeneratedFile(path, content, check);
}
console.log(check ? "Generated contracts are current." : "Generated SDK types, tool and event fixtures, OpenAPI and Web compatibility export.");
}
