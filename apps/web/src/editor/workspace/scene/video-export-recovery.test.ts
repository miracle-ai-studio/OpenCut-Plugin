import { expect, it } from "vitest";
import { ExportVideoRecovery, type ExportVideoAttempt } from "./video-export-recovery";
const identity = (name = "service") => JSON.stringify([`https://${name}.example`, "principal", "credential-id", false]);
const make = (project = "project"): ExportVideoAttempt => ({
  request: { project_id: project, revision: "fixed", sequence_id: "scene", idempotency_key: "original",
    options: { format: "webm", quality: "high", include_audio: false } },
  sceneName: "Scene", phase: "submitting", jobId: null, error: "Private diagnostics are transient",
});
function setup() {
  const data = new Map<string, string>();
  const storage = { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => { data.set(key, value); }, removeItem: (key: string) => { data.delete(key); } };
  return { data, storage, recovery: new ExportVideoRecovery(storage) };
}
it("preserves omitted historical resolution and every explicit resolution through recovery", () => {
  const f = setup();
  for (const resolution of [undefined, "source", "preview540", "p720", "p1080", "p1440", "p2160"] as const) {
    const value = make();
    if (resolution !== undefined) value.request.options.resolution = resolution;
    f.recovery.write(identity(), value);
    const restored = f.recovery.read(identity(), "project")!;
    expect(restored.request).toEqual(value.request);
    expect(Object.hasOwn(restored.request.options, "resolution")).toBe(resolution !== undefined);
  }
  const key = "opencut.export-video-recovery", valid = f.data.get(key)!;
  for (const resolution of [null, "preview360", "p4320", 1080]) {
    const corrupt = JSON.parse(valid);
    corrupt.entries[0].attempt.request.options.resolution = resolution;
    f.data.set(key, JSON.stringify(corrupt));
    expect(() => f.recovery.read(identity(), "project")).toThrow("invalid");
  }
});
it("uses one literal envelope, isolates service/project scope and never persists diagnostic text", () => {
  const f = setup(), attempt = make();
  const historical = JSON.stringify({version:2,entries:[{connection:identity("service-a"),project:"project",attempt:{request:attempt.request,sceneName:attempt.sceneName,phase:attempt.phase,jobId:attempt.jobId}}]});
  f.data.set("opencut.export-video-recovery.v2",historical);
  expect(f.recovery.read(identity("service-a"),"project")?.request).toEqual(attempt.request);expect(f.data.size).toBe(1);
  f.recovery.write(identity("service-a"), make()); f.recovery.write(identity("service-b"), make());
  expect(f.data.get("opencut.export-video-recovery.v2")).toBe(historical);
  expect([...f.data.keys()]).toEqual(["opencut.export-video-recovery.v2","opencut.export-video-recovery"]);
  expect(f.data.get("opencut.export-video-recovery")).not.toContain("diagnostics");
  expect(f.recovery.read(identity("service-a"), "project")?.phase).toBe("unconfirmed");
  expect(f.recovery.read(identity("other"), "project")).toBeNull();
  f.recovery.remove(identity("service-a"), "project");
  expect(f.recovery.read(identity("service-b"), "project")?.request).toEqual(make().request);
  f.recovery.remove(identity("service-b"), "project"); expect(JSON.parse(f.data.get("opencut.export-video-recovery")!).entries).toEqual([]);
});
it("refuses a seventeenth entry before replacing any original request", () => {
  const f = setup(); for (let i = 0; i < 16; i++) f.recovery.write(identity(), make(`project-${i}`));
  const before = [...f.data.values()][0];
  expect(() => f.recovery.write(identity(), make("seventeenth"))).toThrow("local limit");
  expect([...f.data.values()][0]).toBe(before);
  const accepted = make("project-0"); accepted.phase = "accepted"; accepted.jobId = "job";
  f.recovery.write(identity(), accepted); expect(f.recovery.read(identity(), "project-0")?.jobId).toBe("job");
});
it("counts UTF-8 bytes for the total bound and preserves prior entries on overflow", () => {
  const f = setup(); let rejected = false;
  for (let i = 0; i < 16; i++) {
    const saved = make(`${i}${"字".repeat(1000)}`);
    saved.request.revision = "字".repeat(1024); saved.request.sequence_id = "字".repeat(1024);
    saved.request.idempotency_key = "字".repeat(1024); saved.sceneName = "字".repeat(2048);
    const before = [...f.data.values()][0];
    try { f.recovery.write(identity(), saved); }
    catch { rejected = true; expect([...f.data.values()][0]).toBe(before); break; }
  }
  expect(rejected).toBe(true); expect(new TextEncoder().encode([...f.data.values()][0]).byteLength).toBeLessThanOrEqual(128 * 1024);
});
it("rejects unknown persisted fields and duplicate scoped requests", () => {
  const f = setup(); f.recovery.write(identity(), make());
  const key = [...f.data.keys()][0], valid = f.data.get(key)!;
  const withPath = JSON.parse(valid); withPath.entries[0].attempt.source_path = "/private/media";
  f.data.set(key, JSON.stringify(withPath)); expect(() => f.recovery.read(identity(), "project")).toThrow("invalid");
  const duplicate = JSON.parse(valid); duplicate.entries.push(duplicate.entries[0]);
  f.data.set(key, JSON.stringify(duplicate)); expect(() => f.recovery.read(identity(), "project")).toThrow("invalid");
});
it("rejects credentials in URLs and control characters before persistence", () => {
  const f = setup();
  expect(() => f.recovery.write(JSON.stringify(["https://user:secret@example.test", "principal", "credential", false]), make())).toThrow();
  expect(() => f.recovery.write(identity(), make("project\npath"))).toThrow();
  expect(f.data.size).toBe(0);
});
