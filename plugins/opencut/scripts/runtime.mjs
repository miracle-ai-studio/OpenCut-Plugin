import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export const digest = (file) =>
  crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
export const dataDirectory = () =>
  path.resolve(
    process.env.OPENCUT_PLUGIN_DATA_DIR ||
      path.join(os.homedir(), ".opencut-plugin"),
  );
export function workspaceDatabase(config={}) {
  const selected=process.env.OPENCUT_PLUGIN_DATABASE??config.database;
  if(selected!==undefined && (typeof selected!=="string" || !path.isAbsolute(selected)))
    throw new Error("OPENCUT_PLUGIN_DATABASE / database must be an absolute SQLite file path");
  return selected??path.join(dataDirectory(),"workspace.sqlite");
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
  return rendererEnvironment(binary,path.join(root,"bin/libexec"));
}
// Host assembly shared by the verified installed runtime and explicit source CLI.
export async function rendererEnvironment(binary,libexec) {
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 12))
    throw new Error("Node.js 22.12+ is required");
  for (const executable of ["ffmpeg", "ffprobe"]) {
    const result = spawnSync(executable, ["-version"], {
      encoding: "utf8",
      timeout: 10000,
    });
    if (result.status !== 0)
      throw new Error(
        `${executable} is unavailable; install FFmpeg and expose it on PATH`,
      );
  }

  const { chromium } = await import(
    pathToFileURL(
      path.join(
        libexec,
        "classic-audio-renderer/node_modules/playwright-core/index.mjs",
      ),
    ).href
  );
  const browser =
    process.env.OPENCUT_CLASSIC_VISUAL_CHROMIUM ||
    process.env.OPENCUT_CLASSIC_AUDIO_CHROMIUM ||
    chromium.executablePath();
  if (!fs.existsSync(browser))
    throw new Error(
      "Chromium is unavailable. Run: npx playwright@1.63.0 install chromium",
    );
  const environment = {
    ...process.env,
    OPENCUT_CLASSIC_AUDIO_NODE: process.execPath,
    OPENCUT_CLASSIC_VISUAL_NODE: process.execPath,
    OPENCUT_CLASSIC_AUDIO_CHROMIUM: browser,
    OPENCUT_CLASSIC_VISUAL_CHROMIUM: browser,
    OPENCUT_CLASSIC_AUDIO_PACKAGE: path.join(libexec, "classic-audio-renderer"),
    OPENCUT_CLASSIC_VISUAL_PACKAGE: path.join(
      libexec,
      "classic-visual-renderer",
    ),
    OPENCUT_SOURCE_ART_PACKAGE: path.join(
      libexec,
      "classic-source-art-renderer",
    ),
  };
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
