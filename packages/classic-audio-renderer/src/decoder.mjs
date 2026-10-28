import { isVerifiedRangeSource } from "./verified-range-source.mjs";
import { checkAbort, decodedCount, integer, pcmBytes, reject, validateBuffer } from "./validation.mjs";
import { cancellable } from "./runtime.mjs";
import { guardIsoEditLists, guardIsoEditListsSource } from "./iso-edit-list.mjs";
import { validateWindowAdmission, decodeVideoWindows } from "./windowed-decoder.mjs";
import { isWindowedPcmSource } from "./pcm-source.mjs";

export function capturePrimaryAudioTrack(track) {
  const primary = Object.freeze({
    trackId: integer(track.id, "primary audio track ID", 0, 0xffffffff),
    codec: track.codec,
    sampleRate: integer(track.sampleRate, "primary audio sample rate", 1, 384000),
    channels: integer(track.numberOfChannels, "primary audio channels", 1, 64),
  });
  if (typeof primary.codec !== "string" || !/^[a-z0-9][a-z0-9.-]{0,127}$/.test(primary.codec)) reject("UNSUPPORTED_AUDIO_CAPABILITY", "The primary audio track codec is unsupported");
  return primary;
}

async function decodeVideo(track, AudioBufferSink, primary, context, host, admission, signal) {
  let node;
  try {
    const chunks = [];
    let total = 0;
    for await (const { buffer } of new AudioBufferSink(track).buffers(0)) {
      checkAbort(signal); validateBuffer(buffer, admission);
      if (buffer.sampleRate !== primary.sampleRate || buffer.numberOfChannels !== primary.channels) reject("INVALID_DECODED_PCM", "Decoded chunks changed the admitted primary audio format");
      total = decodedCount(total + buffer.length, "video decoded frames", admission.maxFrames);
      chunks.push(buffer);
    }
    if (!chunks.length) reject("EMPTY_DECODED_AUDIO", "The authorized video decoded no audio");
    const sampleRate = chunks[0].sampleRate;
    const channels = Math.min(2, chunks[0].numberOfChannels);
    // Decoder resampling follows the Classic browser algorithm; this is not a
    // timeline placement or authored duration calculation. Timestamp gaps are
    // deliberately concatenated, just as Classic AudioBufferSink export does.
    const outputFrames = decodedCount(Math.ceil(total * (context.sampleRate / sampleRate)), "video resampled frames", admission.maxFrames);
    const offline = host.createOfflineContext(channels, outputFrames, context.sampleRate);
    const native = context.createBuffer(channels, total, sampleRate);
    for (let c = 0; c < channels; c++) {
      const combined = new Float32Array(total);
      let offset = 0;
      for (const chunk of chunks) {
        combined.set(chunk.getChannelData(Math.min(c, chunk.numberOfChannels - 1)), offset);
        offset += chunk.length;
      }
      native.copyToChannel(combined, c);
    }
    node = offline.createBufferSource(); node.buffer = native;
    node.connect(offline.destination); node.start(0);
    return await cancellable(offline.startRendering(), host, signal);
  } finally { node?.disconnect(); }
}

function nativePcmWave(bytes, targetRate) {
  if (bytes.length < 12 || bytes[0] !== 82 || bytes[1] !== 73 || bytes[2] !== 70 || bytes[3] !== 70
    || bytes[8] !== 87 || bytes[9] !== 65 || bytes[10] !== 86 || bytes[11] !== 69) return false;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength), limit = Math.min(bytes.length, 65536);
  for (let offset = 12; offset + 8 <= limit;) {
    const size = view.getUint32(offset + 4, true);
    if (size > bytes.length - offset - 8) return false;
    if (view.getUint32(offset, true) === 0x20746d66) { // fmt
      if (size < 16 || offset + 24 > limit) return false;
      const channels = view.getUint16(offset + 10, true), rate = view.getUint32(offset + 12, true);
      const format = view.getUint16(offset + 8, true), bits = view.getUint16(offset + 22, true);
      const bytesPerSample = format === 1 && bits === 16 ? 2 : format === 3 && bits === 32 ? 4 : 0;
      return bytesPerSample !== 0 && channels >= 1 && channels <= 2 && rate === targetRate
        && view.getUint16(offset + 20, true) === channels * bytesPerSample
        && view.getUint32(offset + 16, true) === rate * channels * bytesPerSample;
    }
    offset += 8 + size + (size & 1);
  }
  return false;
}

export async function decodeResource(resource, bytes, context, host, budget, signal) {
  let input, track, AudioBufferSink, primary = null, releaseUpper, pcmAdmission;
  const rangeInput = isVerifiedRangeSource(bytes);
  try {
    checkAbort(signal);
    // Native-rate signed16/float32 RIFF PCM can reuse
    // observed container chunks without changing Web Audio resampling/codec
    // semantics for short sources or other standalone audio formats.
    const pcmCandidate = !rangeInput && resource.kind === "audio"
      && nativePcmWave(bytes, context.sampleRate);
    if (pcmCandidate) {
      const media = await import("mediabunny");
      input = new media.Input({ source: rangeInput ? bytes.source : new media.BlobSource(new Blob([bytes])), formats: media.ALL_FORMATS });
      track = await cancellable(input.getPrimaryAudioTrack(), host, signal);
      if (track) {
        const observed = capturePrimaryAudioTrack(track);
        if (["pcm-s16", "pcm-f32"].includes(observed.codec) && observed.sampleRate === context.sampleRate && observed.channels <= 2) {
          const proposal = await cancellable(host.admitPcmAudioDecode(resource, observed), host, signal);
          if (!proposal || !["whole", "windowed"].includes(proposal.mode)
            || Object.keys(proposal).length !== (proposal.mode === "whole" ? 1 : 2)
            || Object.keys(proposal).some(key => !["mode", "admission"].includes(key)))
            reject("INVALID_AUDIO_PLAN", "Invalid PCM window negotiation");
          if (proposal.mode === "windowed") {
            pcmAdmission = proposal.admission; primary = observed; AudioBufferSink = media.AudioBufferSink;
            validateWindowAdmission(pcmAdmission, primary, context.sampleRate);
            if (pcmAdmission.mode !== "windowed") reject("INVALID_AUDIO_PLAN", "PCM negotiation changed its selected storage mode");
          }
        }
      }
      if (!pcmAdmission) { input.dispose(); input = undefined; track = undefined; }
    }
    if (resource.kind === "video" || rangeInput) {
      if (rangeInput) await guardIsoEditListsSource(bytes); else guardIsoEditLists(bytes);
      const media = await import("mediabunny");
      checkAbort(signal);
      input = new media.Input({ source: rangeInput ? bytes.source : new media.BlobSource(new Blob([bytes])), formats: media.ALL_FORMATS });
      track = await cancellable(input.getPrimaryAudioTrack(), host, signal);
      if (!track) reject("KNOWN_ABSENT_AUDIO", "The authorized video has no primary audio track");
      primary = capturePrimaryAudioTrack(track);
      AudioBufferSink = media.AudioBufferSink;
    }
    // Metadata inspection uses the same Input/track that will decode. The host
    // admits its real native channel layout before any PCM buffer/sink exists.
    const container = pcmAdmission !== undefined || rangeInput || resource.kind === "video";
    const admission = pcmAdmission ?? await cancellable(container ? host.admitContainerDecode(resource, primary) : host.admitDecode(resource, primary), host, signal);
    if (container) validateWindowAdmission(admission, primary, context.sampleRate);
    integer(admission?.maxFrames, "decoder admission frames", 1);
    integer(admission?.maxChannels, "decoder admission channels", 1, 64);
    if (primary && primary.channels > admission.maxChannels) reject("AUDIO_BUDGET_EXCEEDED", "The selected primary channel layout exceeds decoder admission");
    // Browser codec allocation cannot be bounded by JavaScript alone. Host
    // admission must apply a memory/time quota to the isolated execution realm.
    // Reserve native chunks/copies plus output before invoking the codec.
    releaseUpper = budget.reserve(pcmBytes(container ? admission.workingFrames : admission.maxFrames, admission.maxChannels) * 4);
    checkAbort(signal);
    const buffer = resource.kind === "audio" && !pcmAdmission && !rangeInput
      ? await cancellable(context.decodeAudioData(bytes.slice().buffer), host, signal)
      : await cancellable(admission.mode === "windowed"
        ? decodeVideoWindows(track, AudioBufferSink, primary, context, host, admission, signal)
        : decodeVideo(track, AudioBufferSink, primary, context, host, admission, signal), host, signal);
    if (!isWindowedPcmSource(buffer)) validateBuffer(buffer, admission);
    if (buffer.sampleRate !== context.sampleRate) reject("INVALID_DECODED_PCM", "The browser decoder did not produce the requested target sample rate");
    releaseUpper();
    const release = budget.reserve(isWindowedPcmSource(buffer) ? buffer.residentBytes : pcmBytes(buffer.length, buffer.numberOfChannels));
    return { buffer, release, primary, profile: admission.mode === "windowed" ? "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1"
      : resource.kind === "audio" && !rangeInput ? "web-audio-decode-at-target-rate" : "mediabunny-1.41.0-concatenate-offline-resample" };
  } finally { releaseUpper?.(); input?.dispose(); if (rangeInput) await bytes.close(); }
}
