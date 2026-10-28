import {encodeRgbaBase64} from "../src/validation.mjs";
globalThis.__opencutVisualRgbaBatch=async()=>{};
import test from "node:test";
import assert from "node:assert/strict";
import {
  validateBlendBegin,
  validateGraphicBegin,
} from "../host/protocol.mjs";
import { browserHooks } from "../host/browser.mjs";
import { executeBlendVisualPage } from "../host/page.mjs";
import { blendBegin, graphicObservations } from "./blend-fixture.mjs";
import {
  executeVideoFrame,
  videoBrowserHooks,configureVideoPageCallbacks,
} from "../host/video-browser.mjs";
import { validateVideoBegin } from "../host/video-runner.mjs";

async function configured(packet, request = async () => ({})) {
  const exposed = new Map(),
    calls = [],
    packets = [];
  await browserHooks.configurePage({
    begin: packet,
    signal: new AbortController().signal,
    page: { exposeFunction: async (name, fn) => exposed.set(name, fn) },
    broker: {
      request: (op, args) => {
        calls.push({ op, args });
        return request(op, args);
      },
    },
    writer: { send: async (packet) => packets.push(packet) },
  });
  return {
    callback: exposed.get("__opencutVisualCallback"),
    rgba: exposed.get("__opencutVisualRgba"),
    calls,
    packets,
  };
}
const rgba = { offsetBytes: "0", bytesHex: "ff0000ff" };
test("new begin validates complete fixed wrapper while the graphic operation stays frozen", () => {
  assert.equal(validateBlendBegin(blendBegin()).plan.blends.length, 2);
  assert.throws(() => validateGraphicBegin(blendBegin()));
  for (const change of [
    (p) => p.plan.blends.pop(),
    (p) => (p.plan.execution.identity.revision = "other"),
    (p) => (p.resources = []),
    (p) => (p.fonts = [{ fontId: "invented", handle: "private" }]),
    (p) => (p.admission.maxDecodedPixels = "3"),
  ]) {
    const p = blendBegin();
    change(p);
    assert.throws(() => validateBlendBegin(p));
  }
  const p = blendBegin();
  const old = validateGraphicBegin({
    ...p,
    type: "beginGraphicFrame",
    plan: p.plan.execution,
  });
  assert.equal(
    old.plan.identity.profile,
    "classic-v31-gpu-normal-graphic-frame-v1",
  );
});
test("blend bind waits for the successful graphic reply, rejects injection/replay, then releases RGBA", async () => {
  let graphicDone, blendDone;
  const gate = await configured(validateBlendBegin(blendBegin()), (op) =>
    op === "bindGraphicFrameV1"
      ? new Promise((resolve) => (graphicDone = resolve))
      : op === "bindBlendFrameV1"
        ? new Promise((resolve) => (blendDone = resolve))
        : {},
  );
  assert.throws(() => gate.callback("bindBlendFrameV1", {}));
  await assert.rejects(gate.rgba(rgba));
  await gate.callback("bindCaptionFrameV3", { rasters: [], captions: [] });
  const graphic = gate.callback("bindGraphicFrameV1", graphicObservations);
  assert.throws(() => gate.callback("bindBlendFrameV1", {}));
  graphicDone({});
  await graphic;
  assert.throws(() => gate.callback("bindBlendFrameV1", { blends: [] }));
  const blend = gate.callback("bindBlendFrameV1", {});
  assert.throws(() => gate.callback("bindBlendFrameV1", {}));
  await assert.rejects(gate.rgba(rgba));
  blendDone({});
  await blend;
  await gate.rgba(rgba);
  assert.equal(gate.packets.length, 1);
  assert.deepEqual(
    gate.calls.map((call) => call.op),
    ["bindCaptionFrameV3", "bindGraphicFrameV1", "bindBlendFrameV1"],
  );
  assert.throws(() => gate.callback("bindGraphicFrameV1", graphicObservations));
  assert.throws(
    () =>
      gate.callback("resolveVerifiedBytes", {
        resource: { assetId: "outside" },
      }),
    (error) => error.code === "NOT_AUTHORIZED",
  );
});
test("failed final bind cannot emit RGBA or be concurrently retried", async () => {
  const gate = await configured(
    validateBlendBegin(blendBegin()),
    async (op) => {
      if (op === "bindBlendFrameV1") throw Error("host rejected");
      return {};
    },
  );
  await gate.callback("bindCaptionFrameV3", { rasters: [], captions: [] });
  await gate.callback("bindGraphicFrameV1", graphicObservations);
  await assert.rejects(gate.callback("bindBlendFrameV1", {}), /host rejected/);
  await assert.rejects(gate.rgba(rgba));
  assert.throws(() => gate.callback("bindBlendFrameV1", {}));
});
test("old graphic callback cannot opt into blend; new begin dispatches its own page entry", async () => {
  const p = blendBegin(),
    gate = await configured(
      validateGraphicBegin({
        ...p,
        type: "beginGraphicFrame",
        plan: p.plan.execution,
      }),
    );
  assert.throws(
    () => gate.callback("bindBlendFrameV1", {}),
    (error) => error.code === "INVALID_HOST_CALLBACK",
  );
  let selected;
  await browserHooks.render({
    begin: p,
    page: {
      evaluate: async (fn) => {
        selected = fn;
        return { ok: true, receipt: {} };
      },
    },
  });
  assert.equal(selected, executeBlendVisualPage);
});
test("persistent blend protocol selects the same renderer and reuses byte/font/decoder caches", async t => {
  assert.equal(
    validateVideoBegin({
      ...blendBegin(),
      type: "beginBlendVideo",
      frameCount: 2,
      executionLimit: { kind: "progress", milliseconds: 60000 },
    }).type,
    "beginBlendVideo",
  );
  const previousFetch=globalThis.fetch;globalThis.fetch=async()=>({ok:true});t.after(()=>globalThis.fetch=previousFetch);
  const calls = [];
  globalThis.__opencutVisualModule = {encodeRgbaBase64,
    VisualDecoderCache: class {},
    renderClassicBlendFrame: async (...args) => {
      calls.push(args);
      await args[1].rgba(0,new Uint8Array(4));
      return { blends: [] };
    },
  };
  try {
    for (let n = 0; n < 2; n++)
      assert.deepEqual(
        await executeVideoFrame({
          plan: { n,identity:{width:1,height:1} },
          admission: {},
          profile: "beginBlendFrame",
        }),
        { ok: true, receipt: { blends: [] } },
      );
    for (const index of [4, 5, 6])
      assert.equal(calls[0][index], calls[1][index]);
    assert.equal(calls[0][1].requireResolveCached, true);
  } finally {
    for (const key of [
      "__opencutVisualModule",
      "__opencutVideoBytes",
      "__opencutVideoFonts",
      "__opencutVideoDecoders",
    ])
      delete globalThis[key];
  }
});
test("persistent blend callbacks reset every frame and forbid pixels before that frame's final bind", async () => {
  const exposed = new Map(),
    packets = [];
  const realm = {
    begin: { type: "beginBlendVideo" },
    currentFrame: undefined,
    signal: new AbortController().signal,
    page: { exposeFunction: async (name, fn) => exposed.set(name, fn) },
    broker: { request: async () => ({}) },
    writer: { send: async (packet) => packets.push(packet) },
  };
  const invokeRgba=await configureVideoPageCallbacks(realm);
  const callback = exposed.get("__opencutVisualCallback"),
    emit = packet => invokeRgba("__opencutVisualRgba",[packet]);
  for (let index = 0; index < 2; index++) {
    realm.currentFrame = validateBlendBegin(blendBegin(index));
    await assert.rejects(callback("bindBlendFrameV1", {}));
    await assert.rejects(emit(rgba));
    await callback("bindCaptionFrameV3", { rasters: [], captions: [] });
    await callback("bindGraphicFrameV1", graphicObservations);
    await assert.rejects(emit(rgba));
    await callback("bindBlendFrameV1", {});
    await emit(rgba);
    assert.equal(packets.at(-1).frameIndex, index);
    await assert.rejects(callback("bindBlendFrameV1", {}));
  }
  realm.currentFrame = undefined;
  await assert.rejects(emit(rgba));
});
