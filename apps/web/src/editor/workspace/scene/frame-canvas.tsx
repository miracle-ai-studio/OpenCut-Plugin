import { ErrorNotice } from "../../shared/error-notice";
import { SceneMaskCanvasOverlay, useSceneStack } from "./scene-stack";
import { CanvasTransformOverlay } from "./canvas-transform";
import { useMaskAuthoring } from "./mask/workspace";
import { useCanvasTextAuthoring } from "./text/workspace";
import { CanvasMaskOverlay } from "./mask/canvas-overlay";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useEditingWorkspace as useWorkspace } from "../context";
import { Button } from "../../shared/ui";
import { t, useLocale } from "../../shared/locale";
import { formatTime } from "../../model/time";
import { frameAvailability } from "./frame";
import type { Bookmark } from "./types";
import { CanvasTextOverlay } from "./text/canvas-overlay";
import { usePreviewViewport, ViewportControls } from "../../shortcuts/viewport";
import { Image as ImageIcon, MonitorPlay } from "lucide-react";
import { SnapshotActions } from "./snapshot-actions";
import {createPortal} from "react-dom";
import {Maximize,Minimize} from "lucide-react";

/** Scene pixels and notes are separate layers. Blob URLs live only in the view;
 * authorized task artifacts remain downloadable unchanged through Tasks. */
export function FrameCanvas({
  width,
  height,
  notes,
  controlsTarget,
}: {
  width: number;
  height: number;
  notes: Bookmark[];
  controlsTarget?:HTMLElement|null;
}) {
  const { theme } = useLocale();
  const { controller, api, scene, jobs } = useWorkspace();
  const viewport = usePreviewViewport(
    width,
    height,
    `${scene?.head.project.id}:${scene?.sequenceId}`,
  );
  const maskAuthoring = useMaskAuthoring(),
    textAuthoring = useCanvasTextAuthoring(),
    sceneAuthoring = useSceneStack();
  const canvasEditing =
    !!maskAuthoring?.draft ||
    !!textAuthoring?.canvas ||
    !!sceneAuthoring?.draft;
  const visual = controller.scene.visualPreview;
  const [fullscreen,setFullscreen]=useState(false);
  useEffect(()=>{
    const changed=()=>setFullscreen(!!document.fullscreenElement);
    document.addEventListener("fullscreenchange",changed);return()=>document.removeEventListener("fullscreenchange",changed);
  },[]);
  // Pixels are presentation resources. Only changes in controls/availability
  // enter React; a new video frame must not rebuild the canvas editing overlays.
  const visualControls = useMemo(() => {
    let value = visual.snapshot();
    return () => {
      const next = visual.snapshot();
      if (next.phase !== value.phase || !!next.pixels !== !!value.pixels ||
          next.error !== value.error || next.cacheCompleted !== value.cacheCompleted ||
          next.cacheTotal !== value.cacheTotal||next.surfaceGeneration!==value.surfaceGeneration||next.surfaceActive!==value.surfaceActive||next.target!==value.target) value = next;
      return value;
    };
  }, [visual]);
  const live = useSyncExternalStore(visual.subscribe, visualControls);
  const revision = scene && (scene.historical ?? scene.head).revision;
  const target=JSON.stringify([scene?.head.project.id,revision,scene?.sequenceId]);
  const matchesTarget=!live.target||live.target===target;
  const showingPlaybackSurface=!!live.surfaceGeneration&&!!live.surfaceActive&&matchesTarget;
  const targetRef=useRef(target);targetRef.current=target;
  const canvas = useRef<HTMLCanvasElement>(null);
  const lastCanvasFrame=useRef<{target:string;time:number}|null>(null);
  const playbackCanvas=useRef<HTMLCanvasElement>(null);
  useLayoutEffect(()=>{
    const generation=live.surfaceGeneration??0;
    if(generation)visual.setPlaybackCanvas(playbackCanvas.current,generation);
    return()=>{if(generation)visual.setPlaybackCanvas(null,generation);};
  },[visual,live.surfaceGeneration]);
  const renders=useRef(0);renders.current++;
  useLayoutEffect(() => {
    const draw = () => {
    const current = visual.snapshot(), node = canvas.current;
    const surface = viewport.ref.current;
    if(surface) Object.assign(surface.dataset, {previewPhase:current.phase,
      previewTime:String(current.time??""),previewRuntime:current.runtime??"",
      presentedFrames:String(current.presentedFrames),discardedFrames:String(current.discardedFrames),
      previewStatistics:current.statistics??"",previewLastRun:current.lastRun??""});
    if(playbackCanvas.current)Object.assign(playbackCanvas.current.dataset,{time:String(current.time??""),generation:current.generation??"",runtime:current.runtime??""});
    if (node && current.pixels && (!current.target||current.target===targetRef.current)) {
      if(node.width !== current.pixels.width)node.width = current.pixels.width;
      if(node.height !== current.pixels.height)node.height = current.pixels.height;
      // Keep the owner's bitmap intact so a theme/layout repaint can redraw
      // the paused picture. transferFromImageBitmap consumes it and leaves no
      // reusable pixels when the browser rebuilds the composited surface.
      const context = node.getContext("2d");
      if ("close" in current.pixels) context?.drawImage(current.pixels, 0, 0);
      else context?.putImageData(current.pixels, 0, 0);
      Object.assign(node.dataset,{time:String(current.time??""),generation:current.generation??"",runtime:current.runtime??""});
      if(current.time!==null)lastCanvasFrame.current={target:targetRef.current,time:current.time};
    }
    };
    draw();
    return visual.subscribe(draw);
  }, [visual, !!live.pixels, live.target, matchesTarget, theme, viewport.ref]);
  const [image, setImage] = useState<{ url: string; stamp: string } | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const epoch = useRef(0);
  const stamp = JSON.stringify([
    api?.sdk.baseUrl,
    scene?.head.project.id,
    revision,
    scene?.sequenceId,
    scene?.playhead.ticks,
    scene?.playhead.timebase,
  ]);
  useEffect(() => {
    epoch.current++;
    setImage(null);
    setMenu(null);
    return () => {
      epoch.current++;
      controller.scene.frame.invalidate();
    };
  }, [controller, api, stamp]);
  useEffect(
    () => () => {
      if (image) URL.revokeObjectURL(image.url);
    },
    [image],
  );
  const preparingTime=scene?.audioPreview.request?.start_time??scene?.playhead;
  const keepPreparationFrame=scene?.audioPreview.phase==="loading"&&!!canvas.current&&
    lastCanvasFrame.current?.target===target&&!!preparingTime&&
    Math.abs(lastCanvasFrame.current.time-Number(preparingTime.ticks)/preparingTime.timebase)<1e-9;
  const attempt = scene?.frame;
  const stillTimer=useRef<ReturnType<typeof setTimeout>|null>(null);
  useEffect(()=>{
    if(!scene?.audioPreview.capability?.available||!scene.frameCapability?.frame.available)return;
    const audio=scene.audioPreview;
    if(!["idle","ended","paused"].includes(audio.phase)||(audio.phase==="paused"&&audio.session))return;
    const timer=setTimeout(()=>{
      stillTimer.current=null;
      void controller.scene.visualPreview.showStill().catch(error=>controller.report(error));
    },60);
    stillTimer.current=timer;
    return ()=>{clearTimeout(timer);if(stillTimer.current===timer)stillTimer.current=null;};
  },[controller,api,revision,scene?.sequenceId,scene?.playhead,scene?.audioPreview.phase,scene?.audioPreview.session,scene?.audioPreview.capability?.available,scene?.frameCapability?.frame.available]);
  useEffect(()=>{
    const leave=()=>visual.dispose();window.addEventListener("pagehide",leave);window.addEventListener("beforeunload",leave);
    return()=>{window.removeEventListener("pagehide",leave);window.removeEventListener("beforeunload",leave);leave();};
  },[visual]);
  const job = jobs.find((row) => row.job_id === attempt?.jobId);
  // Polling replaces row objects; a stable publication stamp prevents those
  // replacements from discarding an in-progress authorized download.
  const jobStamp = JSON.stringify([
    job?.job_id,
    job?.state,
    job?.result_unavailable?.code,
    job?.artifacts.map((a) => a.content_hash),
  ]);
  useEffect(() => {
    if (
      job &&
      (job.result_unavailable ||
        job.state === "failed" ||
        job.state === "cancelled")
    ) {
      epoch.current++;
      setImage(null);
      controller.scene.frame.rejectTask(job);
      return;
    }
    if (!scene || !attempt?.jobId || attempt.phase !== "accepted") return;
    const requested = epoch.current;
    void controller.scene.frame
      .observe(jobs)
      .then((value) => {
        if (!value || requested !== epoch.current) return;
        const url = URL.createObjectURL(value.blob);
        setImage({ url, stamp });
      })
      .catch((error) => {
        if (requested === epoch.current) controller.report(error);
      });
  }, [
    controller,
    api,
    stamp,
    attempt?.request.idempotency_key,
    attempt?.jobId,
    jobStamp,
  ]);
  if (!scene) return null;
  const reason = frameAvailability(scene);
  const run = async (action: () => Promise<unknown>) => {
    setPending(true);
    try {
      await action();
    } catch (error) {
      controller.report(error);
    } finally {
      setPending(false);
    }
  };
  const working =
    !!attempt &&
    ["submitting", "accepted", "downloading"].includes(attempt.phase);
  const previewBusy=["loading","playing","stalled","unconfirmed"].includes(scene.audioPreview.phase);
  const controls = (<>
      <div className="toolbar padded editor-frame-actions">
        <Button
          icon={<ImageIcon size={14} />}
          title={t("Render scene frame")}
          disabled={
            pending || !!reason || working || canvasEditing || previewBusy ||
            live.phase === "loading" || !scene.audioPreview.capability?.available || attempt?.phase === "unconfirmed"
          }
          onClick={() => {
            epoch.current++;
            setImage(null);
            if(stillTimer.current!==null){clearTimeout(stillTimer.current);stillTimer.current=null;}
            void run(() => controller.scene.visualPreview.showStill());
          }}
        >
          <span className="workbench-action-label">{t("Render scene frame")}</span>
        </Button>
        <SnapshotActions
          prepareCapture={() => controller.scene.frame.prepareCapture()}
          name={(scene.historical ?? scene.head).project.name}
          disabled={pending || !!reason || working || canvasEditing ||
            previewBusy || live.phase === "loading" || attempt?.phase === "unconfirmed"}
          report={(error) => controller.report(error)}
          menu={menu}
          closeMenu={() => setMenu(null)}
        />
        {attempt && ["unconfirmed", "retryable"].includes(attempt.phase) && (
          <Button
            disabled={pending}
            onClick={() => void run(() => controller.scene.frame.retry())}
          >
            {t("Retry original frame request")}
          </Button>
        )}
        {attempt?.phase === "failed" &&
          job?.state === "succeeded" &&
          !job.result_unavailable && (
            <Button
              disabled={pending}
              onClick={() => {
                const requested = epoch.current;
                void run(async () => {
                  const value = await controller.scene.frame.retryDownload(jobs);
                  if (value && requested === epoch.current)
                    setImage({ url: URL.createObjectURL(value.blob), stamp });
                });
              }}
            >
              {t("Retry frame download")}
            </Button>
          )}
        {attempt?.jobId &&
          (job?.state === "queued" ||
            job?.state === "running" ||
            attempt.phase === "downloading") && (
            <Button
              disabled={pending || job?.cancel_requested}
              onClick={() => {
                epoch.current++;
                setImage(null);
                void run(() => controller.scene.frame.cancel());
              }}
            >
              {t("Cancel scene frame")}
            </Button>
          )}
      </div>
<ViewportControls viewport={viewport}/><Button aria-label={t(fullscreen?"Exit fullscreen":"Fullscreen preview")} title={t(fullscreen?"Exit fullscreen":"Fullscreen preview")} icon={fullscreen?<Minimize size={14}/>:<Maximize size={14}/>} onClick={()=>{void (document.fullscreenElement?document.exitFullscreen():viewport.ref.current?.closest<HTMLElement>(".workbench-player")?.requestFullscreen())?.catch(error=>controller.report(error));}}/></>);
  return (
    <>
      {controlsTarget?createPortal(controls,controlsTarget):controls}
      {reason && (
        <div className="padded">
          <p className="muted" role="status">
            {t(reason)}
          </p>
          <Button
            disabled={pending}
            onClick={() =>
              void run(() => controller.scene.frame.refreshCapability())
            }
          >
            {t("Refresh frame availability")}
          </Button>
        </div>
      )}
      {attempt?.error && (
        <ErrorNotice error={attempt.error} />
      )}
      {attempt?.phase === "unconfirmed" && (
        <p className="padded" role="status">
          {t(
            "The frame submission is not confirmed. Retry the original request to find its task.",
          )}
        </p>
      )}
      {attempt && (
        <small className="padded editor-frame-identity">
          {t("Scene time")}:{" "}
          {formatTime(attempt.request.time)}
        </small>
      )}
      {live.error && (
        <ErrorNotice error={live.error} />
      )}
      <div
        className="editor-canvas"
        ref={viewport.ref}
        tabIndex={0}
        data-preview-phase={live.phase}
        data-preview-error={live.error ?? ""}
        data-canvas-renders={renders.current}
        data-preview-time={live.time ?? ""}
        data-preview-runtime={live.runtime ?? ""}
        data-presented-frames={live.presentedFrames}
        data-discarded-frames={live.discardedFrames}
        data-cache-completed={live.cacheCompleted}
        data-cache-total={live.cacheTotal}
        data-preview-statistics={live.statistics??""}
        data-shortcut-scope="canvas"
        data-panning={viewport.panning || undefined}
        {...viewport.events}
        onContextMenu={(event) => {
          if (canvasEditing) return;
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        style={{ aspectRatio: `${width} / ${height}` }}
        role="region"
        aria-label={t("Canvas notes")}
      >
        {live.phase === "loading" && (
          <span className="editor-preview-cache-status" role="status">
            {t(live.cacheTotal===0?"Checking playback cache…":live.cacheCompleted>=live.cacheTotal?"Resuming prepared playback…":"Preparing smooth playback…")} {live.cacheTotal > 0 ? `${live.cacheCompleted}/${live.cacheTotal}` : ""}
          </span>
        )}
        {(!live.pixels||!matchesTarget)&&!keepPreparationFrame&&!live.surfaceActive && !(live.phase === "idle" && image?.stamp === stamp) && (live.phase==="loading"||working) && (
            <span className="editor-preview-status" role="status">
              <MonitorPlay size={32} strokeWidth={1} aria-hidden="true" />
              {t(
                live.phase === "loading"
                  ? "Preparing scene frame…"
                  : working
                    ? "Preparing scene frame…"
                  : "Preparing scene frame…",
              )}
            </span>
        )}
        <div
          className="editor-viewport-scene"
          style={{
            width: viewport.geometry.width,
            height: viewport.geometry.height,
            transform: `translate(calc(-50% + ${viewport.geometry.pan.x}px), calc(-50% + ${viewport.geometry.pan.y}px))`,
          }}
        >
          {!!live.surfaceGeneration&&<canvas key={live.surfaceGeneration} ref={playbackCanvas}
            className="editor-scene-frame" role="img" aria-label={t("Live scene preview")}
            style={{display:showingPlaybackSurface?"block":"none"}} aria-hidden={!showingPlaybackSurface}
            data-preview-surface="worker" data-time={live.time??""} data-generation={live.generation??""}/>} 
          {(live.pixels&&matchesTarget)||keepPreparationFrame ? (
            <canvas
              role="img"
              ref={canvas}
              className="editor-scene-frame"
              aria-label={t("Live scene preview")}
              style={{display:showingPlaybackSurface?"none":undefined}}
              aria-hidden={showingPlaybackSurface||undefined}
              data-time={live.time ?? ""}
              data-generation={live.generation ?? ""}
              data-runtime={live.runtime ?? ""}
            />
          ) : live.phase === "idle" && image?.stamp === stamp ? (
            <img
              className="editor-scene-frame"
              src={image.url}
              alt={t("Rendered scene frame")}
              onError={() => {
                setImage(null);
                controller.scene.frame.rejectImage();
              }}
            />
          ) : (
            null
          )}
          <CanvasTransformOverlay
            width={width}
            height={height}
            disabled={canvasEditing}
          />
          <CanvasTextOverlay />
          <CanvasMaskOverlay width={width} height={height} />
          <SceneMaskCanvasOverlay width={width} height={height} />
          <div className="editor-note-hud" aria-live="polite">
            {notes.map((b) => (
              <div
                className="editor-canvas-note"
                key={b.id}
                style={{ borderColor: b.color ?? undefined }}
              >
                {b.note}
              </div>
            ))}
          </div>
        </div>
      </div>
      <p className="padded muted editor-frame-help">
        {t(
          "This is a still scene frame. Notes are shown only in the editor and excluded from video export.",
        )}
      </p>
    </>
  );
}
