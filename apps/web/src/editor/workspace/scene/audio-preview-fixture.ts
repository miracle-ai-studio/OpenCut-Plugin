import { vi } from "vitest";
import { setup } from "./fixture";
import { AudioPreview } from "./audio-preview";
import type { AudioOutputProgress } from "./audio-preview/output";
import type {
  AudioPreviewSession,
  AudioPreviewControl,
  AudioPreviewCreate,
} from "./types";

/** Protocol/device-consumer fixture. Actual PCM, WebAudio and permissions are
 * independently exercised by the real browser acceptance owned by the host. */
export async function setupAudioPreview(beforeCreate:()=>Promise<void>=async()=>{}) {
  const f = await setup();
  let state = f.controller.getSnapshot().scene!,
    generation = 1;
  const sessionFor = (
    request: AudioPreviewCreate,
  ): AudioPreviewSession => ({

    session_id: "preview",
    generation: "1",
    overlay_epoch: "0",
    identity: {
      project_id: request.project_id,
      revision: request.revision,
      sequence_id: request.sequence_id,
      sample_rate: request.sample_rate,
      channels: 2,
      profile: "classic-realtime-v31",
    },
    owner_id: "owner",
    lease_id: "lease",
    state: "ready",
    actual_start_sample: "0",
    total_frames: "48000",
    submitted_time: { ticks: "0", timebase: 48000 },
    presentation_time: { ticks: "0", timebase: 48000 },
    output: null,
    consumed_frames: "0",
    produced_through_sample: "8192",
    buffered_frames: "8192",
    presentation_fresh: true,
    stalled: false,
    lease_remaining_ms: "30000",
    expires_in_ms: "120000",
    temporary_gain: null,
    failure_code: null,
    next_event_cursor: "0",
  });
  let session: AudioPreviewSession | null = null;
  const created = new Map<string, AudioPreviewSession>();
  const sessions = new Map<string, AudioPreviewSession>();
  const controls: AudioPreviewControl[] = [],
    requests: AudioPreviewCreate[] = [];
  const receipts = new Map<string, AudioPreviewSession>();
  vi.spyOn(f.api.scene, "audioPreviewCapability").mockResolvedValue({

    available: true,
    unavailable_reason: null,
    profile: "classic-realtime-v31",
    sample_rates: [44100, 48000],
    stream_path: "/api/preview/audio/stream",
    max_sessions_per_database: 4,
    pcm_stage: "mastered",
    max_block_frames: 4096,
    max_buffered_frames: 8192,
    owner_lease_ms: 30000,
    authorization_heartbeat_ms: 1000,
    session_ttl_ms: 120000,
  });
  const create = vi
    .spyOn(f.api.scene, "audioPreviewCreate")
    .mockImplementation(async (r) => {
      requests.push(r);
      session = created.get(r.idempotency_key) ?? {
        ...sessionFor(r),
        session_id: `preview-${created.size}`,
      };
      created.set(r.idempotency_key, session);
      sessions.set(session.session_id, session);
      return structuredClone(session);
    });
  const control = vi
    .spyOn(f.api.scene, "audioPreviewControl")
    .mockImplementation(async (r) => {
      controls.push(r);
      if (receipts.has(r.control_id))
        return structuredClone(receipts.get(r.control_id)!);
      session = sessions.get(r.session_id) ?? null;
      if (!session) throw new Error("Expected session");
      const command = r.command;
      if (command.kind === "play") {
        if (session.state === "paused") {
          session.generation = (BigInt(session.generation) + 1n).toString();
          session.actual_start_sample = session.presentation_time.ticks;
          session.consumed_frames = "0";
          session.output = null;
          session.submitted_time = session.presentation_time;
        }
        session.state = "playing";
      } else if (command.kind === "pause" || command.kind === "stop")
        session.state = command.kind === "pause" ? "paused" : "stopped";
      else if (command.kind === "report_consumed") {
        session.consumed_frames = command.consumed_frames;
        session.submitted_time = {
          ticks: (
            BigInt(session.actual_start_sample) +
            BigInt(command.consumed_frames)
          ).toString(),
          timebase: session.identity.sample_rate,
        };
        if (command.output) {
          session.output = command.output;
          session.presentation_time = { ticks: (BigInt(session.actual_start_sample) + BigInt(command.output.frames)).toString(), timebase: session.identity.sample_rate };
        }
        session.stalled = command.stalled;
      } else if (
        command.kind === "set_temporary_gain" ||
        command.kind === "clear_temporary_gain" ||
        command.kind === "seek"
      ) {
        session.generation = (BigInt(session.generation) + 1n).toString();
        session.actual_start_sample = session.presentation_time.ticks;
        session.consumed_frames = "0";
        session.output = null;
        if (command.kind === "set_temporary_gain")
          session.temporary_gain = command.gain;
        else if (command.kind === "clear_temporary_gain")
          session.temporary_gain = null;
        else {
          // This fixture's returned sample was deliberately chosen by the service.
          session.actual_start_sample = "12345";
          session.presentation_time = {
            ticks: "12345",
            timebase: session.identity.sample_rate,
          };
        }
        if (command.kind !== "seek")
          session.overlay_epoch = (
            BigInt(session.overlay_epoch) + 1n
          ).toString();
        session.submitted_time = session.presentation_time;
      }
      receipts.set(r.control_id, structuredClone(session));
      return structuredClone(session);
    });
  const stream = vi
    .spyOn(f.api.sdk, "openAudioPreviewStream")
    .mockImplementation(async (_input, { signal } = {}) => {
      return new Response(
        new ReadableStream({
          start(controller) {
            signal?.addEventListener("abort", () => {
              try {
                controller.close();
              } catch {
                /* already cancelled */
              }
            });
          },
        }),
        {
          headers: {
            "content-type": "application/vnd.opencut.audio-preview",
          },
        },
      );
    });
  const outputs: {
    silenced: boolean;
    finalFrames: string;
    pushed: Float32Array[];
    pictureLimits: string[];
    progress(p: AudioOutputProgress): void;
    completedBlock(): void;
    failure(error: Error): void;
  }[] = [];
  const preview = new AudioPreview(
    {
      api: () => f.api,
      state: () => state,
      generation: () => generation,
      viewGeneration: () => 0,
      changeView: () => 0,
      publish: (s) => {
        state = s;
      },
      acquire: () => () => {},
      refreshTasks: async () => {},
    },
    (time) => {
      state = { ...state, playhead: time };
    },
    (_token, rate, progress, failure, completedBlock) => {
      const output = {
        silenced: false,
        finalFrames: "0",
        pushed: [] as Float32Array[],
        pictureLimits: [] as string[],
        progress: (p: AudioOutputProgress) => progress({ ...p, output: "output" in p ? p.output : p.stalled && p.consumedFrames === "0" ? null : {
          frames: p.consumedFrames, source: "device_timestamp", uncertainty_frames: 0,
        } }),
        completedBlock,
        failure,
      };
      outputs.push(output);
      return {
        sampleRate: rate,
        ready: Promise.resolve(),
        waitForCredit: async () => {},
        limitPicture: (frames:string) => {output.pictureLimits.push(frames);},
        push: async (pcm) => {
          output.pushed.push(pcm);
        },
        silence: () => {
          output.silenced = true;
        },
        stop: async () => {
          output.progress({ consumedFrames: output.finalFrames, stalled: false, contextTime: 0 });
          output.silenced = true;
          return output.finalFrames;
        },
      };
    },
    beforeCreate,
  );
  await preview.capability();
  return {
    ...f,
    preview,
    controls,
    requests,
    outputs,
    create,
    control,
    stream,
    sessionFor,
    state: () => state,
    update: (patch: Partial<typeof state>) => {
      state = { ...state, ...patch };
    },
    invalidateGeneration: () => {
      generation++;
      preview.invalidate();
    },
    cleanup: () => {
      preview.invalidate();
      f.controller.dispose();
    },
  };
}
