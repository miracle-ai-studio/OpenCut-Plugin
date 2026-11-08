/** Verify the installed release through its current Web UI in the owned MCP fixture. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { chromium, expect } from "@playwright/test";
import { editorSnapshot } from "./project-inspection.mjs";
import { installScenePreviewPumpWitness } from "./verify-scene-preview.mjs";

const { values: args } = parseArgs({ options: {
  workspace: { type: "string" }, archive: { type: "string" }, headed: { type: "boolean", default: false },
} });
assert.ok(args.workspace && args.archive, "Pass --workspace and the exact --archive used by verify-plugin-release.py");
const root = await fs.realpath(args.workspace);
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const read = async file => JSON.parse(await fs.readFile(file, "utf8"));
const packageReport = await read(path.join(root, "verification.json"));
assert.equal(packageReport.status, "passed");
assert.equal(hash(await fs.readFile(args.archive)), packageReport.package_sha256);
const data = path.join(root, "data"), config = await read(path.join(data, "install.json"));
assert.equal(config.database, path.join(data, "workspace.sqlite"));
assert.ok(config.runtime.startsWith(root + path.sep));
const runtime = config.runtime, manifestBytes = await fs.readFile(path.join(runtime, "manifest.json"));
assert.equal(hash(manifestBytes), packageReport.runtime_manifest_sha256);
const manifest = JSON.parse(manifestBytes);
const binary = path.join(runtime, "bin/opencut");
assert.equal(hash(await fs.readFile(binary)), packageReport.binary_sha256);
const installedScripts = path.join(path.dirname(runtime), "plugins/opencut/scripts");
const node = path.join(runtime, "bin/libexec/runtimes/node/bin/node");
const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("OPENCUT_")));
environment.OPENCUT_PLUGIN_DATA_DIR = data;
const reportFile = path.join(root, "desktop-verification.json");
const checkpoint = path.join(root, "desktop-checkpoint.json");
let state;
try { state = await read(checkpoint); } catch (error) {
  if (error.code !== "ENOENT") throw error;
  state = { project_id: "desktop-release-preview", stages: {} };
}
const save = async () => {
  await fs.writeFile(checkpoint + ".pending", JSON.stringify(state, null, 2) + "\n");
  await fs.rename(checkpoint + ".pending", checkpoint);
};
const service = action => {
  const result = JSON.parse(execFileSync(node, [path.join(installedScripts, "web.mjs"), action], {
    env: environment, encoding: "utf8", timeout: 90000, maxBuffer: 2 * 1024 * 1024,
  }));
  assert.equal(result.error, null); return result.data;
};
const report = {
  format: "opencut.desktop-release-acceptance.v1", status: "running",
  package_sha256: packageReport.package_sha256, binary_sha256: packageReport.binary_sha256,
  runtime_manifest_sha256: packageReport.runtime_manifest_sha256,
  verifier_sha256: hash(await fs.readFile(fileURLToPath(import.meta.url))),
  native_renderer: manifest.native_renderer, checks: [],
  scope: "Installed platform package, synthetic UI import/edit, native continuous pixels and PCM, pause/resume/seek, undo/redo/reload and service restart; no physical listening, Finder/Gatekeeper, interrupted-write or performance qualification",
};
await fs.writeFile(reportFile, JSON.stringify(report, null, 2) + "\n");
let browser, page, started = false;
const errors = [];
const record = (name, evidence) => { report.checks.push({ name, evidence }); console.log(name); };
try {
  let running = service("start"); started = true;
  assert.equal(running.ready, true);
  assert.equal(running.database, config.database);
  assert.equal(await fs.realpath(running.executable), await fs.realpath(binary));
  const serviceState = await read(path.join(data, "server/state.json"));
  assert.equal(serviceState.web_root, path.join(runtime, "web"));
  assert.equal(serviceState.executable_sha256, report.binary_sha256);
  browser = await chromium.launch({ headless: !args.headed, executablePath: process.env.OPENCUT_CHROME });
  report.browser = await browser.version(); report.headless = !args.headed;
  let stages;
  const stage = async (name, action) => {
    if (!(name in stages)) { stages[name] = await action(); await save(); }
    record(name, stages[name]); return stages[name];
  };
  page = await browser.newPage({ locale: "en-US", viewport: { width: 1440, height: 1100 } });
  page.setDefaultTimeout(30000);
  page.on("pageerror", error => errors.push(error.message));
  const writes = [];
  page.on("request", request => {
    if (request.url().endsWith("/api/tools/opencut_edit_apply")) writes.push(request.postDataJSON());
  });
  const pumps = [];
  page.on("worker", worker => {
    if (!/pump-worker/.test(worker.url())) return;
    const entry = { worker, installed: worker.evaluate(installScenePreviewPumpWitness, { rolling: true }) };
    void entry.installed.catch(() => {}); pumps.push(entry);
  });
  const witness = async () => {
    const entry = pumps.at(-1);
    if (!entry) return null;
    await entry.installed;
    return entry.worker.evaluate(() => structuredClone(globalThis.__scenePumpWitness));
  };
  const tool = async (name, input) => {
    const envelope = await page.evaluate(async ({ name, input }) => (await fetch(`/api/tools/${name}`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(input),
    })).json(), { name, input });
    assert.equal(envelope.error, null, JSON.stringify(envelope.error)); return envelope.data;
  };
  const inspect = () => editorSnapshot(tool("opencut_project_inspect", { project_id: state.project_id, revision: null }));
  const mediaAction = async action => {
    const planning = page.waitForResponse(response => response.url().endsWith("/api/tools/opencut_media_import_plan"));
    const applying = page.waitForResponse(response => response.url().endsWith("/api/tools/opencut_edit_apply"));
    const [, planned, applied] = await Promise.all([action(), planning, applying]);
    const plan = await planned.json(), receipt = await applied.json();
    assert.equal(plan.error, null); assert.equal(receipt.error, null);
    assert.deepEqual(applied.request().postDataJSON(), plan.data.transaction);
    await expect(page.locator(".save-status")).toHaveText("Saved");
  };
  const open = async () => {
    await page.goto(`${running.endpoint}/editor/${state.project_id}`);
    await expect(page.getByRole("main", { name: "OpenCut editor", exact: true })).toBeVisible();
    const guide = page.getByRole("dialog", { name: "Welcome to OpenCut", exact: true });
    if (await guide.isVisible()) await guide.getByRole("button", { name: "Close", exact: true }).click();
  };
  await page.goto(`${running.endpoint}/editor`);
  await expect(page.getByRole("button", { name: "New project", exact: true })).toBeEnabled();
  // The unchanged create request is safe to replay even after an interrupted setup.
  await tool("opencut_project_create", { project: { id: state.project_id, name: "Desktop release preview",
    sequence_id: "main", width: 160, height: 90, fps: { numerator: 24, denominator: 1 } },
    idempotency_key: "desktop-release-preview-create" });
  await open();
  for (const kind of ["video", "audio"]) {
    const filename = path.join(root, kind === "video" ? "desktop-colors.mp4" : "desktop-tone.wav");
    try { await fs.access(filename); } catch (error) {
      if (error.code !== "ENOENT") throw error;
      const source = kind === "video" ? ["-f", "lavfi", "-i", "color=c=red:size=160x90:rate=24:duration=8",
        "-vf", "drawbox=x=0:y=0:w=iw:h=ih:color=lime:t=fill:enable='mod(floor(t),2)',colorspace=iall=bt601-6-625:all=bt709:fast=1",
        "-c:v", "libx264", "-bf", "0", "-pix_fmt", "yuv420p"]
        : ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=8", "-c:a", "pcm_s16le"];
      execFileSync("ffmpeg", ["-v", "error", ...source, filename], { timeout: 30000 });
    }
    const name = path.basename(filename);
    let snapshot = await inspect();
    let asset = snapshot.project.assets.find(asset => asset.name === name);
    if (!asset) {
      await mediaAction(() => page.getByLabel("Import media files", { exact: true }).setInputFiles(filename));
      snapshot = await inspect(); asset = snapshot.project.assets.find(asset => asset.name === name);
    }
    assert.ok(asset, "UI import must bind the authored source");
    const elements = snapshot.project.sequences.flatMap(sequence => sequence.tracks.flatMap(track => track.elements));
    if (!elements.some(element => element.asset_id === asset.id || element.content?.asset_id === asset.id)) {
      await page.getByRole("button", { name: `More actions for ${name}`, exact: true }).click();
      await page.getByRole("menuitem", { name: "Placement settings", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "Place media", exact: true });
      await dialog.getByRole("combobox", { name: /^Target track/ }).selectOption("new");
      for (const [label, value] of Object.entries({ "Timeline start (seconds)": "0", "Source start (seconds)": "0", "Source duration (seconds)": "8" }))
        await dialog.getByLabel(label, { exact: true }).fill(value);
      if (kind === "video") await dialog.getByLabel("Include source audio", { exact: true }).setChecked(false);
      await mediaAction(() => dialog.getByRole("button", { name: "Add to timeline", exact: true }).click());
      await expect(dialog).toBeHidden();
    }
  }
  let fixed = await inspect();
  state.fixture_revision = fixed.revision; await save();
  report.project_content_sha256 = hash(JSON.stringify(fixed.project));
  const episode = hash(JSON.stringify([report.package_sha256, report.runtime_manifest_sha256,
    report.verifier_sha256, report.browser, report.headless, report.project_content_sha256]));
  stages = state.stages[episode] ??= {};
  record("UI import and reviewed atomic placement", { project_id: state.project_id, revision: fixed.revision,
    sources: await Promise.all(["desktop-colors.mp4", "desktop-tone.wav"].map(async name => ({ name, sha256: hash(await fs.readFile(path.join(root, name))) }))) });
  const live = page.locator('canvas.editor-scene-frame:not([aria-hidden="true"])');
  const seek = async time => {
    await page.locator('summary[aria-label="Go to time"]').click();
    await page.getByLabel("Playhead (seconds)", { exact: true }).fill(String(time));
    await page.getByRole("button", { name: "Go to time", exact: true }).click();
    await page.locator('summary[aria-label="Go to time"]').click();
  };
  const pause = async () => {
    await page.getByRole("button", { name: "Pause scene audio", exact: true }).click();
    await expect(page.locator(".workbench-transport-status")).toHaveText("Scene sound paused");
  };
  await stage("Native continuous pixels and independent 440 Hz PCM", async () => {
    await page.locator('summary[aria-label="Playback options"]').click();
    await page.getByLabel("Audio preview sample rate", { exact: true }).selectOption("48000");
    await page.locator('summary[aria-label="Playback options"]').click();
    await seek(0);
    const beforeWrites = writes.length, began = Date.now(), samples = [];
    let observed = null, firstConsumption = null;
    await page.getByRole("button", { name: "Play scene preview", exact: true }).click();
    await expect.poll(async () => {
      const sample = await live.evaluate(canvas => {
        const time = Number(canvas.dataset.time), generation = canvas.dataset.generation;
        const surface = new OffscreenCanvas(1, 1), context = surface.getContext("2d", { willReadFrequently: true });
        context.drawImage(canvas, Math.floor(canvas.width / 2), Math.floor(canvas.height / 2), 1, 1, 0, 0, 1, 1);
        return { time, generation, rgb: [...context.getImageData(0, 0, 1, 1).data].slice(0, 3), runtime: canvas.dataset.runtime };
      });
      if (sample.time > 0 && !samples.some(old => old.time === sample.time)) samples.push(sample);
      const sound = await witness();
      if (sound?.startObserved && sound.pcm.length >= 1024 && sound.progresses.some(progress => Number(progress.frames) > 0)) {
        observed = sound; firstConsumption ??= Date.now() - began;
      }
      return sample.time;
    }, { timeout: 30000, intervals: [100] }).toBeGreaterThan(4.25);
    await pause();
    assert.ok(observed, "Actual audio pump PCM and Worklet consumption must be observed during playback");
    const pcm = observed.pcm, crossings = [];
    for (let i = 1; i < pcm.length; i++) if (pcm[i - 1] <= 0 && pcm[i] > 0)
      crossings.push(i - 1 - pcm[i - 1] / (pcm[i] - pcm[i - 1]));
    const frequency = (crossings.length - 1) * 48000 / (crossings.at(-1) - crossings[0]);
    const rms = Math.sqrt(pcm.reduce((sum, value) => sum + value * value, 0) / pcm.length);
    expect(Math.abs(frequency - 440)).toBeLessThan(1);
    // Realtime PCM is already mastered. Compare against an independent browser
    // compressor with the documented fixed parameters, including its declared
    // 288-frame lookahead; do not compare against raw-source RMS or fit a gain.
    const sourceBytes = execFileSync("ffmpeg", ["-v", "error", "-i", path.join(root, "desktop-tone.wav"),
      "-af", `atrim=end_sample=${pcm.length + 288}`, "-ac", "1", "-f", "f32le", "-"], { timeout: 30000 });
    const sourcePcm = Array.from(new Float32Array(sourceBytes.buffer.slice(sourceBytes.byteOffset, sourceBytes.byteOffset + sourceBytes.length)));
    const expectedPcm = await page.evaluate(async source => {
      const context = new OfflineAudioContext(2, source.length, 48000);
      const buffer = context.createBuffer(2, source.length, 48000);
      for (let channel = 0; channel < 2; channel++) buffer.copyToChannel(Float32Array.from(source), channel);
      const input = context.createBufferSource(), compressor = context.createDynamicsCompressor(), gain = context.createGain();
      input.buffer = buffer;
      for (const [key, value] of Object.entries({ threshold: -1, knee: 0, ratio: 20, attack: 0.001, release: 0.12 })) compressor[key].value = value;
      gain.gain.value = 0.98;
      input.connect(compressor); compressor.connect(gain); gain.connect(context.destination); input.start();
      return Array.from((await context.startRendering()).getChannelData(0).subarray(288));
    }, sourcePcm);
    assert.equal(expectedPcm.length, pcm.length);
    const maximum = Math.max(...pcm.map((value, index) => Math.abs(value - expectedPcm[index])));
    const signal = expectedPcm.reduce((sum, value) => sum + value * value, 0);
    const noise = pcm.reduce((sum, value, index) => sum + (value - expectedPcm[index]) ** 2, 0);
    const snr = noise ? 10 * Math.log10(signal / noise) : null;
    await fs.writeFile(path.join(root, "desktop-pcm-comparison.json"), JSON.stringify({
      package_sha256: report.package_sha256, revision: fixed.revision, actual: pcm, expected: expectedPcm,
      maximum_absolute_error: maximum, snr_db: snr, reference: "Independent browser OfflineAudioContext with fixed mastering and 288-frame predelay; no fitted gain or time",
    }));
    expect(maximum).toBeLessThanOrEqual(0.000001);
    if (snr !== null) expect(snr).toBeGreaterThanOrEqual(100);
    const interior = samples.filter(sample => sample.time % 1 > 0.15 && sample.time % 1 < 0.85);
    for (let second = 0; second < 4; second++) assert.ok(interior.some(sample => Math.floor(sample.time) === second));
    for (const sample of interior) {
      const expected = Math.floor(sample.time) % 2 ? [0, 255, 0] : [255, 0, 0];
      expect(Math.max(...sample.rgb.map((value, i) => Math.abs(value - expected[i])))).toBeLessThanOrEqual(4);
      const identity = JSON.parse(sample.runtime); assert.equal(identity.renderer, "native");
      assert.equal(identity.rendererSha256, manifest.native_renderer.files["bin/opencut-render-worker"].sha256);
    }
    const at = await live.getAttribute("data-time"); await page.waitForTimeout(250);
    assert.equal(await live.getAttribute("data-time"), at);
    assert.equal((await inspect()).revision, fixed.revision); assert.equal(writes.length, beforeWrites);
    await page.screenshot({ path: path.join(root, "desktop-preview.png"), fullPage: true });
    return { frequency_hz: frequency, rms, sampled_pcm_frames: pcm.length, maximum_pcm_error: maximum, pcm_snr_db: snr, samples: interior,
      first_consumption_observed_ms: firstConsumption, consumed_frames: observed.progresses.at(-1).frames,
      stopped_at: Number(at), scope: "Independent UI canvas pixels and actual pump PCM; no sustained FPS or physical audio claim" };
  });
  await stage("Pause, resume and seek preserve the saved revision", async () => {
    // A new invocation may reuse the previous passed stage with no live session.
    await page.getByRole("button", { name: "Play scene preview", exact: true }).click();
    await expect.poll(async () => Number(await live.getAttribute("data-time")), { timeout: 30000 }).toBeGreaterThan(0.5);
    await pause();
    const at = Number(await live.getAttribute("data-time"));
    await page.getByRole("button", { name: "Play scene preview", exact: true }).click();
    await expect.poll(async () => Number(await live.getAttribute("data-time")), { timeout: 30000 }).toBeGreaterThan(at + 0.25);
    await pause();
    await seek(2.25);
    await expect.poll(async () => Number(await live.getAttribute("data-time")), { timeout: 30000 }).toBe(2.25);
    assert.equal((await inspect()).revision, fixed.revision);
    return { resumed_from: at, seek_seconds: 2.25, revision: fixed.revision };
  });
  await stage("UI rename, undo, redo and reload", async () => {
    const original = await inspect(), name = original.project.name + " checked";
    await page.getByRole("button", { name: `Rename project ${original.project.name}`, exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Rename project", exact: true });
    await dialog.getByRole("textbox", { name: "Project name", exact: true }).fill(name);
    const committed = page.waitForResponse(response => response.url().endsWith("/api/tools/opencut_edit_apply"));
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    assert.equal((await (await committed).json()).error, null);
    await expect(dialog).toBeHidden(); await open();
    assert.deepEqual((await inspect()).project, { ...original.project, name });
    const history = async direction => {
      const applied = page.waitForResponse(response => response.url().endsWith(`/api/tools/opencut_edit_${direction}_apply`));
      await page.getByRole("button", { name: direction === "undo" ? "Undo last edit" : "Redo last edit", exact: true }).click();
      assert.equal((await (await applied).json()).error, null);
      await expect(page.locator(".save-status")).toHaveText("Saved");
    };
    await history("undo"); assert.deepEqual((await inspect()).project, original.project);
    await history("redo"); assert.deepEqual((await inspect()).project, { ...original.project, name });
    await history("undo"); await open();
    const restored = await inspect(); assert.deepEqual(restored.project, original.project);
    return { before_revision: original.revision, restored_revision: restored.revision, project_content_preserved: true };
  });
  fixed = await inspect();
  await page.close(); page = null;
  await browser.close(); browser = null;
  await stage("Service restart retains project and exported job", async () => {
    service("stop"); started = false;
    running = service("start"); started = true; assert.equal(running.ready, true);
    // Explicit local reads after restart use the exact installed binary and owned database.
    const call = (name, input) => {
      const result = JSON.parse(execFileSync(binary, ["--database", config.database, "--json", "call", name, "--input", "-"],
        { input: JSON.stringify(input), encoding: "utf8", timeout: 30000, env: environment }));
      assert.equal(result.error, null); return result.data;
    };
    const restored = await editorSnapshot(call("opencut_project_inspect", { project_id: state.project_id, revision: null }));
    assert.deepEqual(restored, fixed);
    const retained = await read(path.join(root, `checkpoint-${report.binary_sha256.slice(0, 12)}.json`));
    const original = retained.stages["automatic-render-result"];
    const job = call("opencut_job_view", { job_id: original.job_id });
    assert.equal(job.state, "succeeded"); assert.deepEqual(job.artifacts, original.artifacts);
    return { revision: restored.revision, job_id: job.job_id, artifacts: job.artifacts };
  });
  assert.deepEqual(errors, []); report.status = "passed";
} catch (error) {
  report.status = "failed"; report.error = String(error); console.error(error);
  if (page) {
    report.alerts = await page.getByRole("alert").allTextContents().catch(() => []);
    await page.screenshot({ path: path.join(root, "desktop-failure.png"), fullPage: true }).catch(() => {});
  }
  process.exitCode = 1;
} finally {
  if (browser) await browser.close();
  if (started) { try { service("stop"); } catch (error) { report.status = "failed"; report.cleanup_error = String(error); process.exitCode = 1; } }
  report.page_errors = errors;
  await fs.writeFile(reportFile, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ status: report.status, report: reportFile }));
}
