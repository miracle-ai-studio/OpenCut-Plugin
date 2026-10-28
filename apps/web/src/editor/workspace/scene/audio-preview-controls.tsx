import { ErrorNotice, friendlyError } from "../../shared/error-notice";
import { Select } from "../../shared/controls";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useWorkspace } from "../context";
import { Button, Field } from "../../shared/ui";
import { t } from "../../shared/locale";
import { AudioLines, LoaderCircle, Pause, Play } from "lucide-react";
import { usePlayerContext } from "./workbench/player-context";

/** Sound-only transport and opt-in synchronized Classic media preview. */
export function AudioPreviewControls({ compact = false }: { compact?: boolean } = {}) {
  const { controller, scene, api } = useWorkspace();
  const playerContext = usePlayerContext();
  const preview = controller.scene.audioPreview;
  const snapshot = scene && (scene.historical ?? scene.head);
  const identity = `${snapshot?.project.id}/${snapshot?.revision}/${scene?.sequenceId}`;
  const [rate, setRate] = useState(48000);
  const diagnostic=useRef<HTMLSpanElement>(null);
  useLayoutEffect(()=>{
    let lastAt=-Infinity;
    const draw=()=>{const now=performance.now();if(now-lastAt<100)return;lastAt=now;
      if(diagnostic.current)Object.assign(diagnostic.current.dataset,preview.presentationDiagnostics());};
    draw();return preview.subscribePresentation(draw);
  },[preview,identity,scene?.audioPreview.phase]);
  useEffect(() => {
    void preview.capability();
    const departed = () => preview.retireDocument();
    const visibility = () => {
      if (document.hidden) void preview.pause().catch(() => {});
    };
    // Queue the single keepalive while this document still has its transport.
    // pagehide also covers BFCache; retirement itself is idempotent locally.
    window.addEventListener("beforeunload", departed);
    window.addEventListener("pagehide", departed);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      window.removeEventListener("beforeunload", departed);
      window.removeEventListener("pagehide", departed);
      document.removeEventListener("visibilitychange", visibility);
      preview.invalidate();
    };
  }, [preview, api, identity]);
  const capability = scene?.audioPreview.capability;
  const rates =
    capability?.sample_rates.filter((r) => r === 44100 || r === 48000) ?? [];
  useEffect(() => {
    if (rates.length && !rates.some((r) => r === rate)) setRate(rates[0]);
  }, [capability, rate]);
  if (!scene) return null;
  const state = scene.audioPreview;
  const active = ["loading", "playing", "stalled"].includes(state.phase);
  const run = (action: () => Promise<unknown>) => {
    playerContext?.stopExternal();
    const report = (error:unknown) => {
      // Playback owns one inline error. Do not repeat it as a workspace error
      // and another alert after the same rejected preparation promise.
      if(!controller.getSnapshot().scene?.audioPreview.error)controller.report(error);
    };
    try {
      void action().catch(report);
    } catch (e) {
      report(e);
    }
  };
  const status =
    state.phase === "loading"
      ? "Preparing scene sound…"
      : state.phase === "playing"
        ? "Playing scene sound"
        : state.phase === "stalled"
          ? "Buffering scene playback; the playhead is stopped."
          : state.phase === "paused"
            ? "Scene sound paused"
            : state.phase === "ended"
              ? "Scene sound ended"
              : state.phase === "unconfirmed"
                ? "Audio session is unconfirmed. Retry the original request."
                : "Scene sound is stopped";
  if (compact) return <div className="workbench-transport" aria-label={t("Scene sound")}>
    <Button className="workbench-play" aria-label={t(active ? "Pause scene audio" : "Play scene preview")} title={t(active ? "Pause scene audio" : "Play scene preview")}
      disabled={!capability?.available || state.phase === "unconfirmed"}
      onClick={() => run(() => active ? preview.pause() : preview.play(rate, controller.scene.visualPreview.start()))}
      icon={state.phase === "loading" ? <LoaderCircle size={19} className="workbench-loading" /> : active ? <Pause size={19} fill="currentColor" /> : <Play size={19} fill="currentColor" />} />
    <details className="workbench-audio-options"><summary title={t("Playback options")} aria-label={t("Playback options")}><AudioLines size={16} /></summary><div className="workbench-popover">
      <Field label={t("Audio preview sample rate")}><Select value={rate} disabled={active || state.phase === "unconfirmed" || !capability?.available} onChange={event => setRate(Number(event.target.value))}>{rates.map(value => <option key={value} value={value}>{value} Hz</option>)}</Select></Field>
      <Button disabled={!capability?.available || active || state.phase === "unconfirmed"} onClick={() => run(() => preview.play(rate))}>{t("Play scene audio")}</Button>
      <p role="status">{t(status)}</p>
    </div></details>
    <span ref={diagnostic} className="workbench-transport-status" role="status" data-playback-error={state.error??""} data-buffered-frames={state.session?.buffered_frames} data-produced-through-sample={state.session?.produced_through_sample} data-consumed-frames={state.session?.consumed_frames} data-output-frames={state.session?.output?.frames} data-output-clock-source={state.session?.output?.source} data-underflow-events={state.deviceUnderflowEvents} data-underflow-frames={state.deviceUnderflowFrames}>{t(status)}</span>
    {state.phase === "unconfirmed" && <Button onClick={() => run(() => preview.retry())}>{t("Retry original audio session")}</Button>}
    {state.error && <span className="workbench-play-error" role="alert" data-audio-error={state.error}>{friendlyError(state.error)}</span>}
    {state.pictureError && <span className="workbench-play-error" role="alert" data-picture-error={state.pictureError}>{friendlyError(state.pictureError)}</span>}
    {!capability?.available && capability?.unavailable_reason && <span className="workbench-play-error" role="status">{t(capability.unavailable_reason)}</span>}
  </div>;
  return (
    <div className="v4-audio-preview padded" aria-label={t("Scene sound")}>
      <div className="toolbar">
        <Field label={t("Audio preview sample rate")}>
          <Select
            value={rate}
            disabled={
              active || state.phase === "unconfirmed" || !capability?.available
            }
            onChange={(e) => setRate(Number(e.target.value))}
          >
            {rates.map((r) => (
              <option key={r} value={r}>
                {r} Hz
              </option>
            ))}
          </Select>
        </Field>
        <Button
          disabled={
            !capability?.available || active || state.phase === "unconfirmed"
          }
          onClick={() => run(() => preview.play(rate))}
        >
          {t("Play scene audio")}
        </Button>
        <Button
          disabled={
            !capability?.available || active || state.phase === "unconfirmed"
          }
          onClick={() =>
            run(() => preview.play(rate, controller.scene.visualPreview.start()))
          }
        >
          {t("Play scene preview")}
        </Button>
        <Button disabled={!active} onClick={() => run(() => preview.pause())}>
          {t("Pause scene audio")}
        </Button>
        {state.phase === "unconfirmed" && (
          <Button onClick={() => run(() => preview.retry())}>
            {t("Retry original audio session")}
          </Button>
        )}
      </div>
      <p role="status">{t(status)}</p>
      <p className="muted">
        {t(
          "Scene preview follows confirmed sound consumption. Video, static PNG/JPEG, fixed-font text, captions and basic graphics support the available blend modes. Rectangular and elliptical feathered masks are supported on media and graphics. Complex masks and effects require separate rendering support.",
        )}
      </p>
      {!capability?.available && (
        <p>
          {t(
            capability?.unavailable_reason ??
              state.error ??
              "This service does not support live scene audio.",
          )}
        </p>
      )}
      {state.error && capability?.available && (
        <ErrorNotice error={state.error} />
      )}
    </div>
  );
}
