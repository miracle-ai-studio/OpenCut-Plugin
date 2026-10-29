import { ExportSummary, ExportStatus } from "./export-status";
import { ErrorNotice } from "../../shared/error-notice";
import { Select } from "../../shared/controls";
import { useEffect, useState, useSyncExternalStore } from "react";
import { Button, Field, Modal } from "../../shared/ui";
import { t, useLocale } from "../../shared/locale";
import { useWorkspace } from "../context";
import { VideoExport, type ExportVideoOptions } from "./video-export";

export function VideoExportDialog({ exporter, onClose, showTasks, embedded = false, format }: { exporter: VideoExport; onClose(): void; showTasks(): void; embedded?: boolean; format?: ExportVideoOptions["format"] }) {
  useLocale();
  const { controller, api, scene, busy } = useWorkspace(), state = useSyncExternalStore(exporter.subscribe, exporter.getSnapshot);
  const [options, setOptions] = useState<ExportVideoOptions>({ format: "mp4", quality: "high", include_audio: true, resolution: "p1080" });
  const [pending, setPending] = useState(false), [error, setError] = useState<string | null>(null);
  const view = scene?.historical ?? scene?.head;
  useEffect(() => {
    const current = controller.getSnapshot();
    if (!api || !scene || current.api !== api || current.scene?.head.project.id !== scene.head.project.id) return;
    void exporter.refreshCapability().catch(error => {
      const active = controller.getSnapshot();
      if (active.api === api && active.scene?.head.project.id === scene.head.project.id) controller.report(error);
    });
  }, [exporter, controller, api, scene?.head.project.id, scene?.sequenceId, view?.revision]);
  if (!scene || !view) return null;
  const selectedOptions = { ...options, format: format ?? options.format };
  const attempt = state.attempt, captured = attempt?.request.options ?? selectedOptions, unavailable = exporter.unavailable(selectedOptions);
  const run = async (action: () => Promise<unknown>) => {
    setPending(true); setError(null);
    try { await action(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); controller.report(e); } finally { setPending(false); }
  };
  const reset = () => { try { exporter.dismiss(); setError(null); } catch (e) { setError(String(e)); } };
  const body = <>
    <p>{t("Scene")}: {attempt?.sceneName ?? view.project.sequences.find(s => s.id === scene.sequenceId)?.name}</p>
    <ExportSummary view={view} sequenceId={attempt?.request.sequence_id ?? scene.sequenceId} revision={attempt?.request.revision} />
    <p>{t("Exports at the selected resolution, preserving the scene aspect ratio and frame rate. Scene notes are excluded.")}</p>
    <fieldset disabled={busy || pending || !!attempt}>
      {!embedded && <Field label={t("Format")}><Select value={captured.format} onChange={e => {
        const format = e.target.value; if (format === "mp4" || format === "webm") setOptions(current => ({ ...current, format }));
      }}><option value="mp4">MP4 (H.264)</option><option value="webm">WebM (VP9)</option></Select></Field>}
      <Field label={t("Resolution")}><Select value={captured.resolution ?? "source"} onChange={e => {
        const resolution = e.target.value;
        if (resolution === "p720" || resolution === "p1080" || resolution === "p1440" || resolution === "p2160") setOptions(current => ({ ...current, resolution }));
      }}>
        {attempt && (!captured.resolution || captured.resolution === "source") && <option value="source">{t("Original size")}</option>}
        {attempt && captured.resolution === "preview540" && <option value="preview540">540p</option>}
        <option value="p720">720p</option><option value="p1080">1080p</option><option value="p1440">2K (1440p)</option><option value="p2160">4K (2160p)</option>
      </Select></Field>
      <Field label={t("Quality")}><Select value={captured.quality} onChange={e => {
        const quality = e.target.value; if (quality === "low" || quality === "medium" || quality === "high" || quality === "very_high") setOptions(current => ({ ...current, quality }));
      }}><option value="low">{t("Low")}</option><option value="medium">{t("Medium")}</option><option value="high">{t("High")}</option><option value="very_high">{t("Very high")}</option></Select></Field>
      <label><input type="checkbox" checked={captured.include_audio} onChange={e => setOptions(current => ({ ...current, include_audio: e.target.checked }))} />{t("Include audio")}</label>
    </fieldset>
    {unavailable && !attempt && <p role="status">{t(unavailable)}</p>}
    {(error || attempt?.error) && <ErrorNotice error={error ?? attempt?.error} />}
    {attempt?.jobId && <ExportStatus jobId={attempt.jobId} name={attempt.sceneName} />}
    {attempt && <div role="status">
      <p>{t(attempt.phase === "accepted" ? "Your export continues even if you close this window."
        : attempt.phase === "unconfirmed" ? "The submission is not confirmed. Retry the original export to find its task."
        : attempt.phase === "submitting" ? "Starting video export…" : "Export was not accepted.")}</p>
      {attempt.phase === "accepted" && <><Button onClick={() => { onClose(); showTasks(); }}>{t(embedded ? "View export records" : "View tasks")}</Button><Button disabled={busy || pending} onClick={reset}>{t("New video export")}</Button></>}
      {(attempt.phase === "unconfirmed" || attempt.phase === "retryable") && <Button disabled={busy || pending} onClick={() => void run(() => exporter.retry())}>{t("Retry original export")}</Button>}
      {attempt.phase === "retryable" && <Button disabled={busy || pending} onClick={reset}>{t("Reset export")}</Button>}
    </div>}
    <p className="muted">{t(embedded ? "Find progress and published files in Export records." : "You can also find this export in Tasks.")}</p>
    <div className="modal-footer"><Button onClick={onClose}>{t("Close")}</Button>
      <Button className="primary" disabled={busy || pending || state.checking || !!unavailable || !!attempt} onClick={() => void run(() => exporter.start(selectedOptions))}>{t("Start video export")}</Button></div>
  </>;
  return embedded ? body : <Modal title={t("Export video")} onClose={onClose}>{body}</Modal>;
}
