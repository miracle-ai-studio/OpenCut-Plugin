import type { ToolInput } from "../../../lib/editor-sdk";
import { canonicalRecoveryIdentity, validRecoveryId as validId } from "./edit-recovery-pointers";

export type ExportVideoRequest = Extract<ToolInput<"opencut_render_submit">,{kind:"video"}>["request"];
export type ExportVideoOptions = ExportVideoRequest["options"];
export type ExportVideoAttempt = {
  request: ExportVideoRequest;
  sceneName: string;
  phase: "submitting" | "unconfirmed" | "retryable" | "accepted";
  jobId: string | null;
  error: string | null;
};
export type StoragePort = Pick<Storage, "getItem" | "setItem">;
type SavedAttempt = Omit<ExportVideoAttempt, "error">;
type Entry = { connection: string; project: string; attempt: SavedAttempt };
const maxEntries = 16, maxTotal = 128 * 1024, maxRequest = 16 * 1024;
const bytes = (s: string) => new TextEncoder().encode(s).byteLength;
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const keys = (v: Record<string, unknown>, expected: string[]) => Object.keys(v).length === expected.length && expected.every(k => k in v);
const invalid = () => Error("Saved export recovery is invalid or exceeds its local limit.");
const format = (v: unknown): v is ExportVideoOptions["format"] => v === "mp4" || v === "webm";
const quality = (v: unknown): v is ExportVideoOptions["quality"] => v === "low" || v === "medium" || v === "high" || v === "very_high";
const resolution = (v: unknown): v is NonNullable<ExportVideoOptions["resolution"]> => v === "source" || v === "preview540" || v === "p720" || v === "p1080" || v === "p1440" || v === "p2160";
const phase = (v: unknown): v is ExportVideoAttempt["phase"] => v === "submitting" || v === "unconfirmed" || v === "retryable" || v === "accepted";

function attempt(value: unknown): SavedAttempt {
  if (!record(value) || !keys(value, ["request", "sceneName", "phase", "jobId"]) || !record(value.request)) throw invalid();
  const r = value.request, options = r.options;
  if (!keys(r, ["project_id", "revision", "sequence_id", "idempotency_key", "options"]) ||
      !validId(r.project_id) || !validId(r.revision) || !validId(r.sequence_id) || !validId(r.idempotency_key) || !record(options) ||
      !keys(options, ["format", "quality", "include_audio", ...("resolution" in options ? ["resolution"] : [])]) ||
      ("resolution" in options && !resolution(options.resolution)) || !format(options.format) ||
      !quality(options.quality) || typeof options.include_audio !== "boolean" ||
      bytes(JSON.stringify(r)) > maxRequest || typeof value.sceneName !== "string" || value.sceneName.length > 2048 ||
      !phase(value.phase) ||
      (value.jobId !== null && !validId(value.jobId)) || (value.phase === "accepted" && value.jobId === null)) throw invalid();
  return { request: { project_id: r.project_id, revision: r.revision, sequence_id: r.sequence_id, idempotency_key: r.idempotency_key,
    options: { format: options.format, quality: options.quality, include_audio: options.include_audio,
      ...(resolution(options.resolution) ? { resolution: options.resolution } : {}) } },
    sceneName: value.sceneName, phase: value.phase, jobId: value.jobId };
}

/** A single bounded recovery envelope. Connection identity is a public service /
 * principal / credential identifier, never a bearer credential. No media bytes,
 * source locators, snapshots or diagnostic text are retained. */
export class ExportVideoRecovery {
  constructor(private readonly storage?: StoragePort) {}
  private readRaw() { return this.storage ? this.storage.getItem("opencut.export-video-recovery") : sessionStorage.getItem("opencut.export-video-recovery"); }
  private readOriginal() { return this.storage ? this.storage.getItem("opencut.export-video-recovery.v2") : sessionStorage.getItem("opencut.export-video-recovery.v2"); }
  private writeRaw(raw: string) {
    if (this.storage) this.storage.setItem("opencut.export-video-recovery", raw);
    else sessionStorage.setItem("opencut.export-video-recovery", raw);
    if (this.readRaw() !== raw) throw Error("Export recovery storage did not retain the original request.");
  }
  private scope(connection: string | null, project: string) {
    if (!connection || connection.length > 4096 || !validId(project)) throw Error("Authenticate the service before exporting.");
    return canonicalRecoveryIdentity(connection);
  }
  private entries(): Entry[] {
    const current = this.readRaw(), historical = current === null;
    const raw = historical ? this.readOriginal() : current;
    if (raw === null) return [];
    if (raw.length > maxTotal || bytes(raw) > maxTotal) throw invalid();
    const parsed: unknown = JSON.parse(raw);
    if (!record(parsed) || !(historical ? keys(parsed,["version","entries"]) && parsed.version === 2 : keys(parsed,["entries"])) || !Array.isArray(parsed.entries) || parsed.entries.length > maxEntries) throw invalid();
    const scopes = new Set<string>();
    return parsed.entries.map(value => {
      if (!record(value) || !keys(value, ["connection", "project", "attempt"]) || typeof value.connection !== "string" || !validId(value.project)) throw invalid();
      const connection = this.scope(value.connection, value.project);
      const saved = attempt(value.attempt), scope = JSON.stringify([connection, value.project]);
      if (saved.request.project_id !== value.project || scopes.has(scope)) throw invalid();
      scopes.add(scope); return { connection, project: value.project, attempt: saved };
    });
  }
  read(connection: string | null, project: string): ExportVideoAttempt | null {
    const canonical = this.scope(connection, project);
    const saved = this.entries().find(e => e.connection === canonical && e.project === project)?.attempt;
    if (!saved) return null;
    return { ...saved, phase: saved.phase === "submitting" ? "unconfirmed" : saved.phase,
      error: saved.phase === "submitting" ? "The previous export submission must be confirmed." : null };
  }
  write(connection: string | null, value: ExportVideoAttempt) {
    const project = value.request.project_id, canonical = this.scope(connection, project);
    const saved = attempt({ request: value.request, sceneName: value.sceneName, phase: value.phase, jobId: value.jobId });
    const entries = this.entries().filter(e => e.connection !== canonical || e.project !== project);
    entries.push({ connection: canonical, project, attempt: saved });
    const encoded = JSON.stringify({ entries });
    if (entries.length > maxEntries || bytes(encoded) > maxTotal) throw invalid();
    this.writeRaw(encoded);
  }
  remove(connection: string | null, project: string) {
    const canonical = this.scope(connection, project);
    const entries = this.entries().filter(e => e.connection !== canonical || e.project !== project);
    this.writeRaw(JSON.stringify({ entries }));
  }
}
