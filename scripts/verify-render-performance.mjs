#!/usr/bin/env node
// Scoped before/after measurements. Synthetic media and reports stay outside
// the checkout; this does not replace native jobs or whole-editor acceptance.
import fs from "node:fs/promises";
import path from "node:path";
import http from "node:http";
import { fileURLToPath, pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import { chromium } from "../packages/classic-audio-renderer/node_modules/playwright-core/index.mjs";
import { blendBegin, graphicObservations } from "../packages/classic-visual-renderer/tests/blend-fixture.mjs";
import { validateBlendBegin } from "../packages/classic-visual-renderer/host/protocol.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const directory = process.argv[2], baseline = process.argv[3];
if (!directory || !path.isAbsolute(directory) || path.resolve(directory) === root || path.resolve(directory).startsWith(root + path.sep) || !/^[a-f0-9]{7,40}$/.test(baseline ?? ""))
  throw Error("Usage: node scripts/verify-render-performance.mjs <absolute external directory> <baseline commit>");
await fs.mkdir(directory, { recursive: true, mode: 0o700 });
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const source = async (relative, before) => before ? execFileSync("git", ["show", `${baseline}:${relative}`], { cwd: root }) : fs.readFile(path.join(root, relative));
const sample = path.join(directory, "seek-source.mp4");
try { await fs.access(sample); } catch {
  execFileSync("ffmpeg", ["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=94",
    "-c:v", "libx264", "-preset", "fast", "-g", "60", "-bf", "2", "-pix_fmt", "yuv420p", sample]);
}
const media = await fs.readFile(sample), routes = new Map(), identities = {};
const modules = { "decoder.mjs": "packages/classic-visual-renderer/src/decoder.mjs",
  "validation.mjs": "packages/classic-visual-renderer/src/validation.mjs",
  "image-format.mjs": "packages/classic-visual-renderer/src/image-format.mjs",
  "verified-range-source.mjs": "packages/classic-audio-renderer/src/verified-range-source.mjs" };
for (const variant of ["before", "after"]) for (const [name, relative] of Object.entries(modules)) {
  const bytes = await source(relative, variant === "before");
  routes.set(`/${variant}/${name}`, bytes); identities[`${variant}/${relative}`] = hash(bytes);
}
routes.set("/mediabunny.mjs", await fs.readFile(path.join(root, "packages/classic-audio-renderer/node_modules/mediabunny/dist/bundles/mediabunny.mjs")));
routes.set("/noble.mjs", await fs.readFile(path.join(root, "apps/web/node_modules/@noble/hashes/utils.js")));
const imports = { mediabunny: "/mediabunny.mjs" }, scopes = Object.fromEntries(["before", "after"].map(v => [`/${v}/`, {
  "@opencut/classic-audio-renderer/verified-range-source": `/${v}/verified-range-source.mjs`,
}]));
const server = http.createServer((req, res) => {
  if (req.url === "/") { res.setHeader("Content-Type", "text/html"); res.end(`<script type="importmap">${JSON.stringify({ imports, scopes })}</script>`); return; }
  if (req.url === "/sample.mp4") { res.setHeader("Content-Type", "video/mp4"); res.end(media); return; }
  const bytes = routes.get(req.url); res.writeHead(bytes ? 200 : 404, { "Content-Type": "text/javascript" }); res.end(bytes);
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ headless: true, executablePath: process.env.OPENCUT_CLASSIC_VISUAL_CHROMIUM || chromium.executablePath() });
const report = { baseline: execFileSync("git", ["rev-parse", baseline], { cwd: root, encoding: "utf8" }).trim(),
  source_head: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim(),
  candidate_diff_sha256: hash(execFileSync("git", ["diff", "--", "apps/web/src", "packages"], { cwd: root })),
  browser: browser.version(), node: process.version, media_sha256: hash(media), identities,
  scope: "Synthetic real Chromium decoder/byte bridge measurements; not end-to-end export throughput, native authorization, physical playback or whole-product acceptance.",
  seek: [], bridge: [], thumbnail: [] };
const previous = await fs.readFile(path.join(directory, "render-performance.json"), "utf8").then(JSON.parse).catch(() => null);
if (previous?.browser === report.browser && previous.node === report.node && previous.media_sha256 === report.media_sha256 &&
    Object.entries(identities).every(([key, value]) => previous.identities?.[key] === value)) {
  report.seek = previous.seek ?? [];
  report.prior_failures = [...previous.prior_failures ?? [], ...previous.error ? [previous.error] : []];
}
try {
  for (const variant of ["before", "after"]) {
    if (report.seek.some(run => run.variant === variant && run.results?.length === 3)) continue;
    const page = await browser.newPage(); await page.goto(origin);
    const result = await page.evaluate(async ({ variant, contentHash, size }) => {
      const { VisualDecoderCache } = await import(`/${variant}/decoder.mjs`);
      const { openVerifiedRangeSource } = await import(`/${variant}/verified-range-source.mjs`);
      const bytes = new Uint8Array(await (await fetch("/sample.mp4")).arrayBuffer());
      const resource = { assetId: "synthetic", kind: "video", contentHash, size: String(size) };
      const results = [];
      for (const surfaces of [1, 2, 4]) {
        const decoder = new VisualDecoderCache(); let charged = 0, reads = 0, received = 0;
        const source = await openVerifiedRangeSource(resource, {
          open: async () => ({ transferId: "fixed", contentHash, byteLength: String(size), maxChunkBytes: 65536 }),
          read: async (_id, offset, count) => { reads++; received += count; return { offset: String(offset), eof: offset + count === size, bytes: bytes.subarray(offset, offset + count) }; },
          release: async () => {},
        }, { reserve: n => { charged += n; return () => { charged -= n; }; } }, 512 * 1024, undefined, { prefetch: false });
        decoder.keepRange("fixed", source);
        const permit = { maxWidth: 320, maxHeight: 180, maxPixels: "57600", maxDecodedBytes: String(320 * 180 * 4 * surfaces), decoderSurfaces: surfaces };
        const canvas = new OffscreenCanvas(320, 180), context = canvas.getContext("2d", { willReadFrequently: true });
        const samples = [];
        try {
          for (const time of [.25, 1.75, 90.25, 90.5, .25]) {
            const began = performance.now(), beforeReads = reads, beforeBytes = received;
            const observed = await decoder.decode("fixed", source, { requestedTimeSeconds: time, selectedVideoStream: null, eofPolicy: { kind: "noFrame" } }, permit);
            const elapsed = performance.now() - began;
            context.drawImage(observed.canvas, 0, 0);
            const pixels = context.getImageData(0, 0, 320, 180).data;
            const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", pixels));
            samples.push({ time, elapsed_ms: elapsed, pts: observed.actualTimestampSeconds, reads: reads - beforeReads, bytes: received - beforeBytes,
              pixel_sha256: Array.from(digest, b => b.toString(16).padStart(2, "0")).join("") });
          }
        } finally { await decoder.close(); }
        if (charged !== 0) throw Error("Decoder range budget was not released");
        results.push({ surfaces, samples, released_bytes: charged });
      }
      return results;
    }, { variant, contentHash: hash(media), size: media.length });
    report.seek.push({ variant, results: result }); await page.close();
    console.log(JSON.stringify({ phase: "seek", variant, distant_ms: result.map(r => r.samples[2].elapsed_ms) }));
  }
  for (let i = 0; i < 3; i++) for (let j = 0; j < 5; j++) {
    const a = report.seek[0].results[i].samples[j], b = report.seek[1].results[i].samples[j];
    assert.equal(a.pts, b.pts); assert.equal(a.pixel_sha256, b.pixel_sha256);
  }
  // Independent FFmpeg reference for all five requested positions. Compare
  // decoded RGB with the established <= 12 YUV conversion gate, not just peers.
  const page = await browser.newPage(); await page.goto(origin);
  const pixels = await page.evaluate(async ({ contentHash, size }) => {
    const { VisualDecoderCache } = await import("/after/decoder.mjs");
    const { openVerifiedRangeSource } = await import("/after/verified-range-source.mjs");
    const bytes = new Uint8Array(await (await fetch("/sample.mp4")).arrayBuffer()), results = [];
    for (const surfaces of [1, 2, 4]) {
      const decoder = new VisualDecoderCache();
      const source = await openVerifiedRangeSource({ assetId: "synthetic", kind: "video", contentHash, size: String(size) }, {
        open: async () => ({ transferId: "fixed", contentHash, byteLength: String(size), maxChunkBytes: 65536 }),
        read: async (_id, offset, count) => ({ offset: String(offset), eof: offset + count === size, bytes: bytes.subarray(offset, offset + count) }),
        release: async () => {},
      }, { reserve: () => () => {} }, 512 * 1024, undefined, { prefetch: false });
      decoder.keepRange("fixed", source);
      const permit = { maxWidth: 320, maxHeight: 180, maxPixels: "57600", maxDecodedBytes: String(320 * 180 * 4 * surfaces), decoderSurfaces: surfaces };
      const canvas = new OffscreenCanvas(320, 180), c = canvas.getContext("2d", { willReadFrequently: true });
      try { for (const time of [.25, 1.75, 90.25, 90.5]) {
        const observed = await decoder.decode("fixed", source, { requestedTimeSeconds: time, selectedVideoStream: null, eofPolicy: { kind: "noFrame" } }, permit);
        c.drawImage(observed.canvas, 0, 0);
        const rgba = c.getImageData(0, 0, 320, 180).data;
        results.push({ time, surfaces, bytes: new Uint8Array(rgba.buffer).toBase64() });
      } } finally { await decoder.close(); }
    }
    return results;
  }, { contentHash: hash(media), size: media.length });
  report.independent_decoder = [];
  const references = new Map();
  for (const result of pixels) {
    const actual = Buffer.from(result.bytes, "base64"), actualHash = hash(actual);
    for (const run of report.seek)
      assert.equal(run.results.find(r => r.surfaces === result.surfaces).samples.find(s => s.time === result.time).pixel_sha256, actualHash);
    if (!references.has(result.time)) {
      const frame = Math.floor(result.time * 30);
      references.set(result.time, execFileSync("ffmpeg", ["-v", "error", "-i", sample, "-vf", `select=eq(n\\,${frame})`, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"], { maxBuffer: 1024 * 1024 }));
    }
    const expected = references.get(result.time); assert.equal(expected.length, actual.length);
    let max = 0; for (let i = 0; i < expected.length; i++) max = Math.max(max, Math.abs(actual[i] - expected[i]));
    // Preserve failures while still measuring the independent bridge stages.
    // Peer equality is established above for both candidates; it does not turn
    // a failed independent conversion probe into a passing pixel qualification.
    report.independent_decoder.push({ time: result.time, surfaces: result.surfaces, pixel_sha256: actualHash,
      max_rgba_error: max, gate: 12, passed: max <= 12, baseline_and_candidate_identical: true });
  }
  await page.close();
  // Exercise actual versioned browser output callbacks, without GPU/encoder
  // time hiding the bridge cost. Every received native packet is hashed.
  for (const variant of ["before", "after", "after", "before"]) {
    const relative = "packages/classic-visual-renderer/host/video-browser.mjs";
    const original = await source(relative, variant === "before"); identities[`${variant}/${relative}`] = hash(original);
    const rewritten = original.toString().replace(/from "([^"]+)"/g, (_, specifier) => {
      const resolved = specifier.startsWith(".") ? pathToFileURL(path.resolve(root, path.dirname(relative), specifier)).href
        : specifier === "@opencut/classic-audio-renderer/internal-host-runtime" ? pathToFileURL(path.join(root, "packages/classic-audio-renderer/host/shared.mjs")).href : specifier;
      return `from ${JSON.stringify(resolved)}`;
    });
    const { configureVideoPageCallbacks, executeVideoFrame } = await import(`data:text/javascript;base64,${Buffer.from(rewritten).toString("base64")}`);
    const page = await browser.newPage(); await page.goto(origin);
    const handlers = new Map(), frame = validateBlendBegin(blendBegin(0)), digest = createHash("sha256");
    frame.plan.identity.width = 3840; frame.plan.identity.height = 2160;
    let nativeBytes = 0, packets = 0;
    const realm = { begin: { admission: frame.admission }, currentFrame: frame, signal: new AbortController().signal,
      page: { exposeFunction: async (name, handler) => { handlers.set(name, handler); await page.exposeFunction(name, handler); } },
      broker: { request: async () => ({}) }, writer: { send: async packet => {
        assert.equal(packet.frameIndex, 0); assert.equal(packet.offsetBytes, String(nativeBytes % (3840 * 2160 * 4)));
        const bytes = Buffer.from(packet.bytesHex, "hex"); digest.update(bytes); nativeBytes += bytes.length; packets++;
      } } };
    await configureVideoPageCallbacks(realm);
    const callback = handlers.get("__opencutVisualCallback");
    await callback("bindCaptionFrameV3", { rasters: [], captions: [] }); await callback("bindGraphicFrameV1", graphicObservations); await callback("bindBlendFrameV1", {});
    await page.evaluate(async variant => {
      const codec = await import(`/${variant}/validation.mjs`);
      globalThis.__opencutVisualModule = { ...codec, VisualDecoderCache: class {}, renderClassicTextFrame: async (plan, host) => {
        const bytes = new Uint8Array(65536); for (let i = 0; i < bytes.length; i++) bytes[i] = i % 251;
        const length = plan.identity.width * plan.identity.height * 4;
        for (let offset = 0; offset < length; offset += bytes.length) await host.rgba(offset, bytes.subarray(0, Math.min(bytes.length, length - offset)));
        return { complete: true };
      } };
    }, variant);
    const samples = [];
    for (let n = 0; n < 3; n++) {
      const began = performance.now(), result = await page.evaluate(executeVideoFrame, { plan: { identity: { width: 3840, height: 2160 } }, admission: {} });
      assert.equal(result.ok, true); samples.push(performance.now() - began);
    }
    report.bridge.push({ variant, samples_ms: samples, native_bytes: nativeBytes, packets, native_sha256: digest.digest("hex") });
    await page.close(); console.log(JSON.stringify({ phase: "bridge", variant, samples_ms: samples }));
  }
  assert.equal(new Set(report.bridge.map(r => r.native_sha256)).size, 1);
  const thumbnailPage = await browser.newPage(); await thumbnailPage.goto(origin);
  report.thumbnail = await thumbnailPage.evaluate(async () => {
    const { hexToBytes } = await import("/noble.mjs"), bytes = new Uint8Array(256 * 1024);
    for (let i = 0; i < bytes.length; i++) bytes[i] = i % 256;
    const hex = bytes.toHex(), results = [];
    for (const variant of ["before", "after", "after", "before"]) {
      const start = performance.now(); let decoded;
      for (let i = 0; i < 12; i++) decoded = variant === "before" ? Uint8Array.from(hex.match(/../g), pair => Number.parseInt(pair, 16)) : hexToBytes(hex);
      if (decoded.some((value, i) => value !== bytes[i])) throw Error("JPEG byte conversion changed bytes");
      results.push({ variant, bytes: bytes.length, iterations: 12, elapsed_ms: performance.now() - start });
    }
    return results;
  });
  await thumbnailPage.close();
  report.status = report.independent_decoder.every(value => value.passed) ? "passed" : "performance_compared_with_open_pixel_gate";
} catch (error) { report.status = "failed"; report.error = error.stack; throw error; }
finally {
  await fs.writeFile(path.join(directory, "render-performance.json"), JSON.stringify(report, null, 2) + "\n");
  await browser.close(); await new Promise(resolve => server.close(resolve));
}
console.log(JSON.stringify({ status: report.status, report: path.join(directory, "render-performance.json") }));
