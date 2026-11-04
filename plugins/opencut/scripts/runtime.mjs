import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";

export const digest = (file) =>
  crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
export const dataDirectory = () =>
  path.resolve(
    process.env.OPENCUT_PLUGIN_DATA_DIR ||
      path.join(os.homedir(), ".opencut-plugin"),
  );
function canonicalPath(file) {
  try {
    return fs.realpathSync(file);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return path.join(canonicalPath(path.dirname(file)), path.basename(file));
  }
}
export function workspaceDatabase(config = {}, explicit) {
  const selected = explicit ?? (config.database === undefined
    ? path.join(dataDirectory(), "workspace.sqlite")
    : config.database);
  if (typeof selected !== "string" || !path.isAbsolute(selected))
    throw new Error("The OpenCut workspace database must be an absolute SQLite file path");
  return canonicalPath(selected);
}
export function readConfig() {
  const file = path.join(dataDirectory(), "install.json");
  if (!fs.existsSync(file))
    throw new Error(
      "Install the release runtime first: node scripts/install.mjs. See the GitHub README.",
    );
  const config = JSON.parse(fs.readFileSync(file, "utf8"));
  if (
    config.format !== "opencut.plugin-install.v1" ||
    !path.isAbsolute(config.runtime)
  )
    throw new Error("Invalid local installation configuration");
  return config;
}
export function verifyRuntime(root) {
  root = fs.realpathSync(root);
  const manifest = JSON.parse(
    fs.readFileSync(path.join(root, "manifest.json"), "utf8"),
  );
  const resolved = fs.realpathSync(root);
  for (const [name, expected] of Object.entries(manifest.files)) {
    const file = path.resolve(root, name);
    if (
      !file.startsWith(resolved + path.sep) ||
      !fs.realpathSync(file).startsWith(resolved + path.sep)
    )
      throw new Error("Runtime path escapes installation");
    if (
      fs.statSync(file).size !== expected.size ||
      digest(file) !== expected.sha256
    )
      throw new Error(`Runtime file differs: ${name}`);
  }
  for (const [name, target] of Object.entries(manifest.links || {})) {
    const file = path.resolve(root, name);
    if (
      !file.startsWith(resolved + path.sep) ||
      fs.readlinkSync(file) !== target ||
      !fs.realpathSync(file).startsWith(resolved + path.sep)
    )
      throw new Error("Unsafe runtime link");
  }
  return manifest;
}
export async function runtimeEnvironment(root) {
  const manifest = JSON.parse(
    fs.readFileSync(path.join(root, "manifest.json"), "utf8"),
  );
  const target = `${process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch}-apple-darwin`;
  if (process.platform !== "darwin" || manifest.target !== target)
    throw new Error(
      `This runtime targets ${manifest.target}; current system is ${process.platform}/${process.arch}`,
    );
  const binary = path.join(root, "bin/opencut");
  if (digest(binary) !== manifest.binary_sha256)
    throw new Error("OpenCut executable identity differs");
  root = fs.realpathSync(root);
  function checkedFile(name) {
    const expected = manifest.files[name];
    const file = path.resolve(root, name);
    if (!expected || !file.startsWith(root + path.sep) ||
        !fs.realpathSync(file).startsWith(root + path.sep) ||
        fs.statSync(file).size !== expected.size || digest(file) !== expected.sha256)
      throw new Error(`Runtime file differs: ${name}`);
    return file;
  }
  const native = manifest.native_renderer;
  if (native?.format !== "opencut.native-renderer" || native.target !== target ||
      native.cli !== "bin/opencut" || native.worker !== "bin/opencut-render-worker")
    throw new Error("A native renderer installation is required");
  for (const [name, expected] of Object.entries(native.files)) {
    if (manifest.files[name]?.sha256 !== expected.sha256 || manifest.files[name]?.size !== expected.size)
      throw new Error(`Native renderer identity differs: ${name}`);
    checkedFile(name);
  }
  const bundle = JSON.parse(fs.readFileSync(checkedFile("bin/libexec/runtime-bundle.json"), "utf8"));
  if (bundle.format !== "opencut.native-runtime-bundle")
    throw new Error("A native tool bundle is required");
  const environment = { ...process.env, OPENCUT_RENDER_WORKER: checkedFile(native.worker) };
  const variables = { ffmpeg: "OPENCUT_FFMPEG", ffprobe: "OPENCUT_FFPROBE",
    python: "OPENCUT_FONT_DERIVE_PYTHON", node: "OPENCUT_CLASSIC_VISUAL_NODE" };
  for (const [component, variable] of Object.entries(variables)) {
    const entry = bundle.components[component]?.entry;
    if (typeof entry !== "string") throw new Error(`Missing native runtime component: ${component}`);
    environment[variable] = checkedFile("bin/libexec/" + entry);
  }
  return rendererEnvironment(binary, environment);
}
// Native host assembly shared by installed and explicit source CLI entrypoints.
export async function rendererEnvironment(binary, inherited = process.env) {
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 12))
    throw new Error("Node.js 22.12+ is required");
  const environment = Object.fromEntries(Object.entries(inherited).filter(([key]) =>
    !key.startsWith("OPENCUT_CLASSIC_") && !key.startsWith("DYLD_") &&
    key !== "OPENCUT_SOURCE_ART_PACKAGE"));
  // Existing catalog tools still use this host variable for their Node executable.
  environment.OPENCUT_CLASSIC_VISUAL_NODE = inherited.OPENCUT_CLASSIC_VISUAL_NODE || process.execPath;
  const probe = spawnSync(binary, ["--json", "system", "capabilities"], {
    env: environment,
    encoding: "utf8",
    timeout: 20000,
    maxBuffer: 2 * 1024 * 1024,
  });
  if (probe.status !== 0)
    throw new Error(
      "Installed OpenCut cannot execute: " +
        (
          probe.stderr ||
          probe.error?.message ||
          probe.signal ||
          "unknown failure"
        ).slice(-512),
    );
  if (JSON.parse(probe.stdout).error)
    throw new Error("Installed capability probe was refused");
  return environment;
}
