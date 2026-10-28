import assert from "node:assert/strict";
import test from "node:test";
import { mixCompiledBlock, sampleLinear } from "../src/mixing.mjs";
import { CLASSIC_MASTERING, createClassicMasteringChain, masterWholeScene } from "../src/mastering.mjs";
import { decodedCount, PcmBudget, validateBuffer } from "../src/validation.mjs";
import { capturePrimaryAudioTrack, decodeResource } from "../src/decoder.mjs";
import { validateBegin } from "../host/runner.mjs";
import { guardIsoEditLists } from "../src/iso-edit-list.mjs";
import { cancellable } from "../src/runtime.mjs";
import { prepareVoice } from "../src/preparation.mjs";

function buffer(channels, sampleRate = 44100) {
  const data = channels.map(c => new Float32Array(c));
  return { length: data[0].length, numberOfChannels: data.length, sampleRate, getChannelData: c => data[c], copyToChannel: (values, c) => data[c].set(values) };
}
const position = (lower, numerator = "0", denominator = "1") => ({ lower: String(lower), fractionNumerator: numerator, fractionDenominator: denominator });

test("exact large positions remain EOF; upper interpolation tap clamps only at actual decoded end", () => {
  const data = new Float32Array([0.25, -0.5, 0.75]);
  assert.equal(sampleLinear(data, position(0, "1", "2")), -0.125);
  assert.equal(sampleLinear(data, position(2, "3", "4")), 0.75);
  assert.equal(sampleLinear(data, position("18446744073709551615", "1", "2")), 0);
  assert.throws(() => sampleLinear(data, position(0, "2", "2")), { code: "INVALID_AUDIO_PLAN" });
});

test("source channels, independent sample gain, zero tail and stored mix order reach PCM", () => {
  const output = buffer([[0, 0, 0], [0, 0, 0]]);
  const sources = new Map([["a", buffer([[0.25, 0.75]])], ["b", buffer([[0.5, -0.25], [-0.5, 0.25], [99, 99]])]]);
  mixCompiledBlock(output, sources, ["a", "b"], { startSample: "0", frameCount: 3, voices: [
    { elementId: "a", blockOffset: 0, frameCount: 3, positions: [position(0), position(0, "1", "2"), position(2)], amplitudes: [2, 0.5, 1] },
    { elementId: "b", blockOffset: 1, frameCount: 2, positions: [position(0), position(1)], amplitudes: [0.5, 2] },
  ] }, 0, 3);
  assert.deepEqual([...output.getChannelData(0)], [0.5, 0.5, -0.5]);
  assert.deepEqual([...output.getChannelData(1)], [0.5, 0, 0.5]);
  assert.throws(() => mixCompiledBlock(output, sources, ["a", "b"], { startSample: "0", frameCount: 1, voices: [{ elementId: "b", blockOffset: 0, frameCount: 1, positions: [position(0)], amplitudes: [1] }, { elementId: "a", blockOffset: 0, frameCount: 1, positions: [position(0)], amplitudes: [1] }] }, 0, 1), { code: "INVALID_AUDIO_PLAN" });
});

test("maintain-pitch at unit rate keeps the preparation Float32 rounding stage", async () => {
  const context = { sampleRate: 44100, createBuffer: (channels, frames) => buffer(Array.from({ length: channels }, () => Array(frames).fill(0))) };
  const host = { compilePitchPreparation: async () => ({ mode: "prepared-linear", numChannels: 1, decodedSampleRate: 44100, targetSampleRate: 44100, outputFrames: "1" }), compileSourcePositions: async () => [position(0, "1", "3")] };
  const result = await prepareVoice({ elementId: "a" }, buffer([[0, 1]]), context, host, new PcmBudget(1024));
  assert.equal(result.buffer.getChannelData(0)[0], 0.3333333432674408);
  result.release();
});

test("whole-scene gate uses the peak, and the real WebAudio node parameters remain Classic", async () => {
  const low = buffer([[0.25, -0.5]]);
  const result = await masterWholeScene(low, { createOfflineContext() { throw new Error("must bypass compressor"); } }, new PcmBudget(1024));
  assert.equal(result.buffer, low); assert.equal(result.applied, false);
  const connections = [];
  const node = name => ({ name, gain: {}, connect(to) { connections.push([name, to.name]); }, disconnect() {} });
  const limiter = { ...node("compressor"), ...Object.fromEntries(["threshold", "knee", "ratio", "attack", "release"].map(k => [k, {}])) };
  let gains = 0;
  createClassicMasteringChain({ createGain: () => node(gains++ ? "output" : "input"), createDynamicsCompressor: () => limiter }, node("destination"));
  assert.deepEqual(connections, [["input", "compressor"], ["compressor", "output"], ["output", "destination"]]);
  assert.deepEqual([limiter.threshold.value, limiter.knee.value, limiter.ratio.value, limiter.attack.value, limiter.release.value], [-1, 0, 20, 0.001, 0.12]);
  assert.equal(CLASSIC_MASTERING.headroom, 0.98);
});

test("budget rejects before allocation; cancellation waits for actual host termination", async () => {
  const budget = new PcmBudget(8), release = budget.reserve(8);
  assert.throws(() => budget.reserve(4), { code: "AUDIO_BUDGET_EXCEEDED" });
  release(); release(); assert.equal(budget.used, 0);
  const controller = new AbortController();
  let terminate, acknowledged = false;
  const termination = new Promise(resolve => { terminate = () => { acknowledged = true; resolve(); }; });
  const work = cancellable(new Promise(() => {}), { abortRendering: () => termination }, controller.signal);
  controller.abort(); await Promise.resolve(); assert.equal(acknowledged, false);
  terminate(); await assert.rejects(work, { code: "CANCELLED" });
});

test("actual decoded allocation and accumulated video chunks fail as budget errors, while invalid PCM remains distinct", async () => {
  const admission = { maxFrames: 3, maxChannels: 64 }, budget = new PcmBudget(4096);
  const host = { admitDecode: async () => admission };
  const context = { sampleRate: 44100, decodeAudioData: async () => buffer([[0, 0, 0, 0]]) };
  await assert.rejects(decodeResource({ kind: "audio" }, new Uint8Array([0]), context, host, budget), { code: "AUDIO_BUDGET_EXCEEDED" });
  assert.equal(budget.used, 0, "Failed decoding releases its upper-bound reservation");
  const chunk = buffer([[0, 0]]);
  validateBuffer(chunk, admission);
  const first = decodedCount(chunk.length, "video decoded frames", admission.maxFrames);
  assert.throws(() => decodedCount(first + chunk.length, "video decoded frames", admission.maxFrames), { code: "AUDIO_BUDGET_EXCEEDED" });
  assert.throws(() => validateBuffer(buffer([[0], [0]]), { maxFrames: 3, maxChannels: 1 }), { code: "AUDIO_BUDGET_EXCEEDED" });
  assert.throws(() => validateBuffer(buffer([[]]), admission), { code: "INVALID_DECODED_PCM" });
  assert.throws(() => validateBuffer(buffer([[NaN]]), admission), { code: "INVALID_DECODED_PCM" });
});

test("primary format is captured immutably before admission; audio byte decoding carries no selected container track", async () => {
  const track = { id: 2, codec: "pcm-f32", sampleRate: 48000, numberOfChannels: 2 };
  const primary = capturePrimaryAudioTrack(track);
  track.id = 1; track.numberOfChannels = 1;
  assert.deepEqual(primary, { trackId: 2, codec: "pcm-f32", sampleRate: 48000, channels: 2 });
  assert(Object.isFrozen(primary));
  assert.throws(() => capturePrimaryAudioTrack({ ...track, id: 0x100000000 }), { code: "INVALID_AUDIO_PLAN" });
  assert.throws(() => capturePrimaryAudioTrack({ ...track, codec: null }), { code: "UNSUPPORTED_AUDIO_CAPABILITY" });
  const budget = new PcmBudget(4096);
  let admitted = false;
  const result = await decodeResource({ assetId: "audio", kind: "audio" }, new Uint8Array([0]),
    { sampleRate: 44100, decodeAudioData: async () => { assert(admitted); return buffer([[0.25, 0.5]]); } },
    { admitDecode: async (resource, observed) => { assert.equal(resource.assetId, "audio"); assert.equal(observed, null); admitted = true; return { maxFrames: 2, maxChannels: 1 }; } }, budget);
  assert.equal(result.primary, null);
  result.release(); assert.equal(budget.used, 0);
  // Non-native-rate/float WAV must retain the original byte-decoder path,
  // even when the new private capability is available.
  for (const [format, rate, bits] of [[1,48000,16], [3,48000,32], [1,44100,24]]) {
    const bytes = new Uint8Array(44), data = new DataView(bytes.buffer);
    bytes.set(new TextEncoder().encode("RIFF"),0); bytes.set(new TextEncoder().encode("WAVEfmt "),8);
    data.setUint32(16,16,true); data.setUint16(20,format,true); data.setUint16(22,1,true);
    data.setUint32(24,rate,true); data.setUint32(28,rate*bits/8,true); data.setUint16(32,bits/8,true); data.setUint16(34,bits,true);
    const decoded = await decodeResource({assetId:"audio",kind:"audio"}, bytes,
      {sampleRate:44100,decodeAudioData:async()=>buffer([[.25,.5]])},
      {admitPcmAudioDecode:async()=>assert.fail("Unqualified formats cannot select PCM windows"),
        admitDecode:async(_resource, observed)=>{assert.equal(observed,null);return{maxFrames:2,maxChannels:1};}}, budget);
    assert.equal(decoded.profile,"web-audio-decode-at-target-rate"); decoded.release();
  }
});

test("Rust null/full and selected range plans retain their exact output count before browser launch", () => {
  const begin = {
    executionId: "fixed-range", type: "begin", resources: [], runtime: {},
    admission: { maxPcmBytes: 1048576, maxEncodedBytes: 33554432, maxRssBytes: "104857600", deadlineMs: 15000, quotaProfile: "monitored-terminate" },
    plan: { identity: { projectId: "p", revision: "r", sequenceId: "s", profile: "classic-export-v31-web-audio", sampleRate: 48000 },
      frameCount: "24017", outputFrameCount: "24017", range: null, encodedSource: { mode: "verified-range", cacheBytes: 65536 }, resources: [], voices: [] },
  };
  assert.equal(validateBegin(begin).plan.range, null);
  begin.plan.range = { startSample: "3000", frameCount: "120" }; begin.plan.outputFrameCount = "120";
  assert.deepEqual(validateBegin(begin).plan.range, { startSample: "3000", frameCount: "120" });
  begin.plan.outputFrameCount = "24017";
  assert.throws(() => validateBegin(begin), { code: "INVALID_HOST_PROTOCOL" });
});

function isoBox(type, payload, sizeKind = "normal") {
  const header = sizeKind === "extended" ? 16 : 8;
  const result = Buffer.alloc(header + payload.length);
  result.writeUInt32BE(sizeKind === "extended" ? 1 : sizeKind === "zero" ? 0 : result.length);
  result.write(type, 4, 4, "ascii");
  if (header === 16) result.writeBigUInt64BE(BigInt(result.length), 8);
  result.set(payload, header); return result;
}
function isoEdits(entries, version = 0, sizeKind = "normal") {
  const stride = version === 1 ? 20 : 12, payload = Buffer.alloc(8 + stride * entries.length);
  payload[0] = version; payload.writeUInt32BE(entries.length, 4);
  entries.forEach(([duration, time, rate = 65536], i) => {
    const at = 8 + i * stride;
    if (version === 1) { payload.writeBigUInt64BE(BigInt(duration), at); payload.writeBigInt64BE(BigInt(time), at + 8); }
    else { payload.writeUInt32BE(duration, at); payload.writeInt32BE(time, at + 4); }
    payload.writeInt32BE(rate, at + stride - 4);
  });
  return isoBox("moov", isoBox("trak", isoBox("edts", isoBox("elst", payload))), sizeKind);
}
test("ISO capability gate preserves single edits and leading empties, including extended and terminal-size boxes", () => {
  guardIsoEditLists(Buffer.from("RIFF\0\0\0\0WAVE"));
  guardIsoEditLists(new Uint8Array([0x1a, 0x45, 0xdf, 0xa3, 0x81, 0]));
  for (const version of [0, 1]) for (const size of ["normal", "extended", "zero"]) {
    guardIsoEditLists(isoEdits([[0, 7, 0], [100, -1, 0], [200, -1], [100, version === 1 ? 9007199254740993n : 10]], version, size));
  }
});
test("ISO capability gate rejects discarded semantics and malformed bounded metadata before codec allocation", async () => {
  for (const version of [0, 1]) {
    for (const entries of [[[100, 0], [100, 10]], [[100, 0], [100, -1]], [[100, 0, 32768]]]) {
      const bytes = isoEdits(entries, version);
      assert.throws(() => guardIsoEditLists(bytes), { code: "UNSUPPORTED_AUDIO_CAPABILITY" });
      // Passing through the production decoder must reject before any offline
      // context or actual MediaBunny codec is created.
      await assert.rejects(decodeResource({ kind: "video" }, bytes, { sampleRate: 48000 },
        { admitDecode: async () => ({ maxFrames: 32, maxChannels: 2 }) }, new PcmBudget(8192)), { code: "UNSUPPORTED_AUDIO_CAPABILITY" });
    }
  }
  assert.throws(() => guardIsoEditLists(isoEdits([[1, 0]], 2)), { code: "UNSUPPORTED_AUDIO_CAPABILITY" });
  const huge = isoBox("moov", Buffer.alloc(0), "extended"); huge.writeBigUInt64BE(0xffffffffffffffffn, 8);
  assert.throws(() => guardIsoEditLists(huge), { code: "INVALID_AUDIO_CONTAINER" });
  const malformed = isoEdits([[1, 0]]); malformed.writeUInt32BE(7, 0);
  assert.throws(() => guardIsoEditLists(malformed), { code: "INVALID_AUDIO_CONTAINER" });
  assert.throws(() => guardIsoEditLists(Buffer.concat([isoEdits([[1, 0]]), Buffer.alloc(3)])), { code: "INVALID_AUDIO_CONTAINER" });
  assert.throws(() => guardIsoEditLists(Buffer.concat([isoEdits([]), ...Array.from({ length: 2048 }, () => isoBox("free", Buffer.alloc(0)))])), { code: "UNSUPPORTED_AUDIO_CAPABILITY" });
});


test("WAV mixer bounds block unions separately from actual voice samples and rejects malformed ranges before writing", () => {
  const names = Array.from({ length: 33 }, (_, i) => `e${i}`);
  const sources = new Map(names.map(id => [id, buffer([[.25]])]));
  const voice = (elementId, count = 1) => ({ elementId, blockOffset: 0, frameCount: count, positions: Array(count).fill(position(0)), amplitudes: Array(count).fill(1) });
  const output = buffer([Array(4096).fill(0), Array(4096).fill(0)]);
  const block = voices => ({ startSample: "0", frameCount: 4096, voices });
  assert.throws(() => mixCompiledBlock(output, sources, names, block(names.map(id => voice(id))), 0, 4096), { code: "INVALID_AUDIO_PLAN" });
  assert.throws(() => mixCompiledBlock(output, sources, names, block(names.slice(0, 17).map(id => voice(id, 4096))), 0, 4096), { code: "AUDIO_BUDGET_EXCEEDED" });
  for (const change of [{ frameCount: "1" }, { frameCount: 0 }, { blockOffset: 4096 }, { blockOffset: -1 }, { blockOffset: 4095, frameCount: 2 }]) {
    assert.throws(() => mixCompiledBlock(output, sources, names, block([voice("e0"), { ...voice("e1"), ...change }]), 0, 4096), { code: "INVALID_AUDIO_PLAN" });
  }
  assert(output.getChannelData(0).every(value => value === 0), "Invalid ranges/work never partially mix output");
  mixCompiledBlock(output, sources, names, block(names.slice(0, 16).map(id => voice(id, 4096))), 0, 4096);
  assert(output.getChannelData(0).every(value => value === 4), "Exactly 65536 actual voice samples are admitted");
});

test("fixed plan metadata admits 256 voices while CAS resource admission remains 32", () => {
  const resource = i => ({ assetId: `a${i}`, kind: "audio", contentHash: "a".repeat(64), size: 1 });
  const begin = { executionId: "metadata", type: "begin", runtime: {},
    admission: { maxPcmBytes: 1048576, maxEncodedBytes: 33554432, maxRssBytes: "104857600", deadlineMs: 15000, quotaProfile: "monitored-terminate" },
    plan: { identity: { projectId: "p", revision: "r", sequenceId: "s", profile: "classic-export-v31-web-audio", sampleRate: 48000 }, frameCount: "300", encodedSource: { mode: "verified-range", cacheBytes: 65536 }, resources: Array.from({ length: 32 }, (_, i) => resource(i)),
      voices: Array.from({ length: 256 }, (_, i) => ({ elementId: `e${i}`, assetId: `a${i % 32}` })) },
    resources: Array.from({ length: 32 }, (_, i) => ({ assetId: `a${i}`, handle: `h${i}` })) };
  assert.equal(validateBegin(begin).plan.voices.length, 256);
  const extraVoice = structuredClone(begin); extraVoice.plan.voices.push({ elementId: "extra", assetId: "a0" });
  assert.throws(() => validateBegin(extraVoice), { code: "INVALID_HOST_PROTOCOL" });
  const extraResource = structuredClone(begin); extraResource.plan.resources.push(resource(32)); extraResource.resources.push({ assetId: "a32", handle: "h32" });
  assert.throws(() => validateBegin(extraResource), { code: "INVALID_HOST_PROTOCOL" });
});


test("native-rate float32 RIFF negotiates the existing PCM path and short inputs keep Web Audio",async()=>{
 const bytes=new Uint8Array(52),v=new DataView(bytes.buffer);
 bytes.set(new TextEncoder().encode("RIFF"));v.setUint32(4,44,true);bytes.set(new TextEncoder().encode("WAVEfmt "),8);
 v.setUint32(16,16,true);v.setUint16(20,3,true);v.setUint16(22,1,true);v.setUint32(24,44100,true);
 v.setUint32(28,176400,true);v.setUint16(32,4,true);v.setUint16(34,32,true);bytes.set(new TextEncoder().encode("data"),36);v.setUint32(40,8,true);v.setFloat32(44,.25,true);v.setFloat32(48,.5,true);
 let probes=0;const result=await decodeResource({assetId:"f32",kind:"audio"},bytes,
  {sampleRate:44100,decodeAudioData:async()=>buffer([[.25,.5]])},
  {admitPcmAudioDecode:async(_r,p)=>{assert.equal(p.codec,"pcm-f32");assert.equal(p.channels,1);probes++;return{mode:"whole"};},admitDecode:async(_r,p)=>{assert.equal(p,null);return{maxFrames:2,maxChannels:1};}},new PcmBudget(4096));
 assert.equal(probes,1);assert.equal(result.profile,"web-audio-decode-at-target-rate");result.release();
});
