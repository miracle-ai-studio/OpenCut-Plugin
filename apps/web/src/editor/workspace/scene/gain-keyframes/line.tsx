import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useEditingWorkspace as useWorkspace } from "../../context";
import type { EditorApi } from "../../api";
import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import type {
  KeyframeEdit,
  ScalarChannel,
  ScalarKey,
  ExactTime,
} from "../keyframes/types";
import { seconds } from "../keyframes/types";
import { Button } from "../../../shared/ui";
import type { Element as TimelineElement } from "../types";
import { editRecoveryBlocked } from "../edit-recovery";
import { audioGainAvailability } from "../audio-gain";
import {
  gainChannel,
  sampleTimes,
  axisBounds,
  dragKeys,
  boundedTimeDelta,
  localPlayhead,
  curveEdits,
  authoringPath,
} from "./model";
import { useGainText } from "./messages";
import "./gain-keyframes.css";
type Sample = ToolOutput<"opencut_keyframe_sample">["samples"][number];
type Scope = {
  api: EditorApi;
  connection: string | null;
  project: string;
  revision: string;
  scene: string;
  trackId: string;
  element: TimelineElement;
  channel: ScalarChannel;
  serial: number;
};
type Gesture = {
  scope: Scope;
  pointer: number;
  node: Element;
  from: { x: number; y: number };
  selected: Set<string>;
  whole: boolean;
  edits: KeyframeEdit[];
  rect: DOMRect;
  precise: boolean;
};
export function GainKeyframeLine({
  elementId,
  onSelect,
  compact = false,
}: {
  elementId: string;
  compact?: boolean;
  onSelect?(elementId: string): void;
}) {
  const { api, scene, controller } = useWorkspace(),
    t = useGainText(),
    surface = useRef<SVGSVGElement>(null);
  const [pixelWidth, setPixelWidth] = useState(300),
    [view, setView] = useState<"all" | "start" | "end">("start"),
    [samples, setSamples] = useState<Sample[] | null>(null),
    [sampleScope, setSampleScope] = useState<Scope | null>(null),
    [selected, setSelected] = useState<Set<string>>(new Set()),
    [draft, setDraft] = useState<KeyframeEdit[] | null>(null),
    [pending, setPending] = useState(false),
    [error, setError] = useState<string | null>(null);
  const epoch = useRef(0),
    abort = useRef<AbortController | null>(null),
    drag = useRef<Gesture | null>(null),
    writing = useRef(false),
    measured = useRef({ width: 0, height: 0 });
  const available = scene ? audioGainAvailability(scene, elementId) : null,
    element = available?.element,
    channel = element ? gainChannel(element) : null;
  const blocked = () => {
    const s = controller.getSnapshot(),
      v = s.scene,
      target = v ? audioGainAvailability(v, elementId) : null;
    return (
      !v ||
      !target ||
      !!target.reason ||
      s.busy ||
      !!v.historical ||
      !!v.gainDraft ||
      !!v.propertyDrafts?.length ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        v.audioPreview.phase,
      ) ||
      editRecoveryBlocked(v.recovery) ||
      controller.scene.recovery.blocked() ||
      !!v.recovery?.record ||
      !!v.recovery?.error ||
      ["reading", "staging", "applying"].includes(v.recovery?.phase ?? "") ||
      ["applying", "unconfirmed"].includes(
        v.timelineEdit.attempt?.phase ?? "",
      ) ||
      ["applying", "unconfirmed"].includes(v.clipboard.attempt?.phase ?? "") ||
      !!(v.audioGain && v.audioGain.phase !== "saved")
    );
  };
  const current = (scope: Scope) => {
    const s = controller.getSnapshot(),
      v = s.scene,
      a = v ? audioGainAvailability(v, elementId) : null;
    return (
      epoch.current === scope.serial &&
      s.api === scope.api &&
      s.api.connectionIdentity === scope.connection &&
      v?.head.project.id === scope.project &&
      (v.historical ?? v.head).revision === scope.revision &&
      v.sequenceId === scope.scene &&
      !!a &&
      JSON.stringify(a.element) === JSON.stringify(scope.element)
    );
  };
  const cancelDrag = () => {
    const g = drag.current;
    drag.current = null;
    if (g?.node.hasPointerCapture?.(g.pointer))
      g.node.releasePointerCapture(g.pointer);
    setDraft(null);
  };
  const invalidate = () => {
    epoch.current++;
    abort.current?.abort();
    cancelDrag();
    if (!writing.current) setPending(false);
  };
  useEffect(() => {
    invalidate();
    setError(null);
    if (
      sampleScope &&
      (sampleScope.api !== api ||
        sampleScope.connection !== api?.connectionIdentity ||
        sampleScope.project !== scene?.head.project.id ||
        sampleScope.scene !== scene?.sequenceId ||
        sampleScope.element.id !== elementId)
    ) {
      setSamples(null);
      setSampleScope(null);
      setSelected(new Set());
    }

    if (!api || !scene || !element || !channel) return;
    const scope: Scope = {
      api,
      connection: api.connectionIdentity,
      project: scene.head.project.id,
      revision: (scene.historical ?? scene.head).revision,
      scene: scene.sequenceId,
      trackId: available!.track.id,
      element: structuredClone(element),
      channel: structuredClone(channel),
      serial: epoch.current,
    };
    const times = sampleTimes(element.timeline_range.duration, channel.keys);
    if (times.length > 1024) {
      setError("Volume automation has too many visible keys for this editor.");
      return;
    }
    const stop = new AbortController();
    abort.current = stop;
    setPending(true);
    void (async () => {
      try {
        const capabilities = await editorHostCapabilities(api);
        if (
          !capabilities.tools.includes("opencut_keyframe_sample") ||
          !capabilities.tools.includes("opencut_keyframe_plan")
        )
          throw Error("Automation editing is unavailable on this host.");
        const result = await api.sdk.call(
          "opencut_keyframe_sample",
          {
            project_id: scope.project,
            revision: scope.revision,
            sequence_id: scope.scene,
            channel_id: channel.id,
            times,
          },
          { signal: stop.signal },
        );
        if (stop.signal.aborted || !current(scope)) return;
        if (
          result.revision !== scope.revision ||
          result.channel_id !== channel.id ||
          result.samples.length !== times.length ||
          result.samples.some(
            (s, i) =>
              !Number.isFinite(s.value) ||
              !Number.isFinite(s.physical_value) ||
              s.time.ticks !== times[i].ticks ||
              s.time.timebase !== times[i].timebase,
          )
        )
          throw Error("The sampled gain curve differs from the fixed channel.");
        setSampleScope(scope);
        setSamples(result.samples);
      } catch (cause) {
        if (!stop.signal.aborted && scope.serial === epoch.current)
          setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        if (scope.serial === epoch.current) setPending(false);
      }
    })();
    return () => invalidate();
  }, [
    api,
    api?.connectionIdentity,
    scene?.head.project.id,
    scene?.head.revision,
    scene?.historical?.revision,
    scene?.sequenceId,
    elementId,
  ]);
  useEffect(() => {
    const node = surface.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      const r = node.getBoundingClientRect(),
        previous = measured.current;
      measured.current = { width: r.width, height: r.height };
      if (r.width > 0) setPixelWidth(r.width);
      if (
        previous.width &&
        (previous.width !== r.width || previous.height !== r.height)
      )
        cancelDrag();
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [!!samples]);
  useEffect(() => {
    const cancel = () => {
      if (!writing.current) {
        abort.current?.abort();
        cancelDrag();
        setPending(false);
      }
    };
    const key = (event: KeyboardEvent) => {
      if (
        event.key === "Escape" &&
        (drag.current || pending) &&
        !writing.current
      ) {
        event.preventDefault();
        event.stopPropagation();
        cancel();
      }
    };
    window.addEventListener("blur", cancel);
    window.addEventListener("pagehide", cancel);
    window.addEventListener("keydown", key, true);
    return () => {
      window.removeEventListener("blur", cancel);
      window.removeEventListener("pagehide", cancel);
      window.removeEventListener("keydown", key, true);
    };
  }, [pending]);
  if (!available || !element || !channel || !scene) return null;
  const bounds = axisBounds(channel, samples ?? []),
    duration = seconds(element.timeline_range.duration),
    height = compact ? 36 : 128,
    padding = compact ? 6 : 12,
    width = pixelWidth,
    timeWidth = compact ? width : Math.max(1, width - padding * 2);
  const focused = !compact && channel.keys.length > 1 && view !== "all";
  const start = focused
    ? seconds(channel.keys[view === "start" ? 0 : channel.keys.length - 2].time)
    : 0;
  const end = focused
    ? seconds(channel.keys[view === "start" ? 1 : channel.keys.length - 1].time)
    : duration;
  const visibleDuration = Math.max(1 / 120000, end - start);
  const visibleSamples = (samples ?? []).filter(
    (sample) => seconds(sample.time) >= start && seconds(sample.time) <= end,
  );
  const x = (time: ExactTime) =>
      (compact ? 0 : padding) +
      ((seconds(time) - start) / visibleDuration) * timeWidth,
    y = (value: number) =>
      padding +
      ((bounds.max - value) / (bounds.max - bounds.min)) *
        (height - padding * 2);
  const volumeLabel = (value: number) =>
    bounds.unit === "linear"
      ? `${(value * 100).toFixed(0)}%`
      : `${value.toFixed(1)} dB`;
  const draftMap = new Map(
      (draft ?? []).flatMap((edit) =>
        edit.kind === "put_scalar" ? [[edit.key.id, edit.key] as const] : [],
      ),
    ),
    keys = channel.keys.map((key) => draftMap.get(key.id) ?? key);
  const submit = async (scope: Scope, edits: KeyframeEdit[]) => {
    if (writing.current || blocked() || !edits.length || !current(scope)) {
      setDraft(null);
      return;
    }
    const request: ToolInput<"opencut_keyframe_plan"> = {
      project_id: scope.project,
      expected_revision: scope.revision,
      idempotency_key: crypto.randomUUID(),
      label: "Edit volume automation",
      sequence_id: scope.scene,
      edits: structuredClone(edits),
    };
    const stop = new AbortController();
    abort.current = stop;
    setPending(true);
    setError(null);
    try {
      const tx = await scope.api.sdk.call("opencut_keyframe_plan", request, {
        signal: stop.signal,
      });
      if (stop.signal.aborted || !current(scope)) return;
      if (
        tx.project_id !== request.project_id ||
        tx.expected_revision !== request.expected_revision ||
        tx.idempotency_key !== request.idempotency_key ||
        tx.label !== request.label ||
        tx.operations.length !== 1 ||
        tx.operations[0].type !== "element_put" ||
        tx.operations[0].element.id !== scope.element.id ||
        tx.operations[0].sequence_id !== scope.scene ||
        tx.operations[0].track_id !== scope.trackId
      )
        throw Error(
          "The gain-keyframe plan differs from the original gesture.",
        );
      writing.current = true;
      await controller.scene.applyPlanned(tx, scope.scene);
    } catch (cause) {
      if (!stop.signal.aborted && scope.serial === epoch.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      writing.current = false;
      if (scope.serial === epoch.current) {
        setPending(false);
        setDraft(null);
      }
    }
  };
  const down = (event: ReactPointerEvent, key?: ScalarKey) => {
    event.stopPropagation();
    if (
      event.button !== 0 ||
      pending ||
      !sampleScope ||
      blocked() ||
      !current(sampleScope) ||
      drag.current
    )
      return;
    event.preventDefault();
    const group = key
      ? event.shiftKey
        ? new Set([...selected, key.id])
        : selected.has(key.id)
          ? new Set(selected)
          : new Set([key.id])
      : new Set(channel.keys.map((k) => k.id));
    setSelected(group);
    setError(null);
    onSelect?.(elementId);
    const rect = surface.current!.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = {
      scope: sampleScope,
      pointer: event.pointerId,
      node: event.currentTarget,
      from: { x: event.clientX, y: event.clientY },
      selected: group,
      whole: !key,
      edits: [],
      rect,
      precise: event.altKey,
    };
  };
  const move = (event: ReactPointerEvent) => {
    const g = drag.current;
    if (!g || g.pointer !== event.pointerId) return;
    event.preventDefault();
    event.stopPropagation();
    if (!current(g.scope)) {
      cancelDrag();
      return;
    }
    const timeDelta = g.whole
      ? { ticks: "0", timebase: 1 }
      : boundedTimeDelta(
          g.scope.channel.keys,
          g.selected,
          element.timeline_range.duration,
          ((event.clientX - g.from.x) / timeWidth) *
            visibleDuration *
            (g.precise ? 0.2 : 1),
        );
    let valueDelta =
      (-(event.clientY - g.from.y) / (height - padding * 2)) *
      (bounds.max - bounds.min) *
      (g.precise ? 0.2 : 1);
    const values = g.scope.channel.keys
      .filter((k) => g.selected.has(k.id))
      .map((k) => k.value);
    valueDelta = Math.max(
      bounds.min - Math.min(...values),
      Math.min(bounds.max - Math.max(...values), valueDelta),
    );
    g.edits = dragKeys(g.scope.channel, g.selected, timeDelta, valueDelta);
    setDraft(g.edits);
  };
  const up = (event: ReactPointerEvent) => {
    if (drag.current?.pointer !== event.pointerId) return;
    move(event);
    const g = drag.current;
    drag.current = null;
    if (!g) return;
    if (g.node.hasPointerCapture?.(g.pointer))
      g.node.releasePointerCapture(g.pointer);
    const changed = g.edits.filter(
      (edit) =>
        edit.kind === "put_scalar" &&
        JSON.stringify(edit.key) !==
          JSON.stringify(
            g.scope.channel.keys.find((k) => k.id === edit.key.id),
          ),
    );
    void submit(g.scope, changed);
  };
  const addAtTime = (time: ExactTime) => {
    if (!sampleScope || blocked() || !current(sampleScope)) return;
    if (
      seconds(time) < 0 ||
      seconds(time) > duration ||
      channel.keys.some(
        (key) =>
          BigInt(key.time.ticks) * BigInt(time.timebase) ===
          BigInt(time.ticks) * BigInt(key.time.timebase),
      )
    )
      return;
    void submit(sampleScope, [
      {
        kind: "insert_scalar",
        channel_id: channel.id,
        key_id: crypto.randomUUID(),
        time,
      },
    ]);
  };
  return (
    <section
      className={`editor-gain-keyframes${compact ? " editor-gain-keyframes-compact" : ""}`}
      data-timeline-no-gesture
      onPointerDown={(event) => event.stopPropagation()}
      aria-label={t("Volume automation")}
      onClick={(event) => event.stopPropagation()}
    >
      {!compact && (
        <div className="gain-curve-heading">
          <strong>{t("Volume curve")}</strong>
          <span>
            {channel.keys.length} {t("control points")}
          </span>
        </div>
      )}
      {!compact && (
        <div
          className="gain-curve-view"
          role="group"
          aria-label={t("Curve view")}
        >
          {(["all", "start", "end"] as const).map((value) => (
            <Button
              key={value}
              aria-pressed={view === value}
              disabled={
                !!drag.current ||
                pending ||
                (value !== "all" && channel.keys.length < 2)
              }
              onClick={() => setView(value)}
            >
              {t(
                value === "all"
                  ? "Whole clip"
                  : value === "start"
                    ? "Opening"
                    : "Ending",
              )}
            </Button>
          ))}
          <span>
            {start.toFixed(2)}–{end.toFixed(2)} s
          </span>
        </div>
      )}
      {samples && sampleScope && (
        <svg
          ref={surface}
          viewBox={`0 0 ${width} ${height}`}
          preserveAspectRatio="none"
          aria-label={t("Gain keyframes")}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={cancelDrag}
          onLostPointerCapture={() => {
            if (drag.current) cancelDrag();
          }}
          onDoubleClick={(event) => {
            event.stopPropagation();
            if ((event.target as Element).closest("[data-gain-handle=key]"))
              return;
            const r = event.currentTarget.getBoundingClientRect();
            const time =
              start +
              ((((event.clientX - r.left) / r.width) * width -
                (compact ? 0 : padding)) /
                timeWidth) *
                visibleDuration;
            addAtTime({
              ticks: String(
                Math.round(Math.max(0, Math.min(duration, time)) * 120000),
              ),
              timebase: 120000,
            });
          }}
        >
          {draft || (pending && sampleScope.revision !== (scene.historical ?? scene.head).revision) ? (
            <path
              className="gain-sampled-curve"
              d={authoringPath(
                keys,
                channel.extrapolation,
                start,
                end,
                (time) =>
                  x({
                    ticks: String(Math.round(time * 120000)),
                    timebase: 120000,
                  }),
                y,
              )}
              fill="none"
              vectorEffect="non-scaling-stroke"
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          ) : (
            <polyline
              className="gain-sampled-curve"
              points={visibleSamples
                .map((s) => `${x(s.time)},${y(s.physical_value)}`)
                .join(" ")}
              fill="none"
              vectorEffect="non-scaling-stroke"
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          )}
          <polyline
            role="button"
            aria-label={t("Move volume automation")}
            data-gain-handle="curve"
            points={visibleSamples
              .map((s) => `${x(s.time)},${y(s.physical_value)}`)
              .join(" ")}
            fill="none"
            stroke="transparent"
            strokeWidth="12"
            vectorEffect="non-scaling-stroke"
            onPointerDown={(event) => down(event)}
          />
          {keys
            .filter(
              (key) =>
                (seconds(key.time) >= start && seconds(key.time) <= end) ||
                draftMap.has(key.id),
            )
            .map((key) => (
              <g
                key={key.id}
                role="button"
                aria-label={`${t("Volume point")} ${seconds(key.time).toFixed(3)} s · ${volumeLabel(key.value)}`}
                aria-pressed={selected.has(key.id)}
                data-key-id={key.id}
                data-gain-handle="key"
                className={selected.has(key.id) ? "selected" : ""}
                onPointerDown={(event) => down(event, key)}
              >
                <title>
                  {seconds(key.time).toFixed(3)} s · {volumeLabel(key.value)}
                </title>
                <circle
                  className="gain-key-hit"
                  cx={Math.max(7, Math.min(width - 7, x(key.time)))}
                  cy={y(key.value)}
                  r={11}
                />
                <circle
                  className="gain-key-dot"
                  cx={Math.max(7, Math.min(width - 7, x(key.time)))}
                  cy={y(key.value)}
                  r={4.5}
                  vectorEffect="non-scaling-stroke"
                />
              </g>
            ))}
        </svg>
      )}
      <p>
        {t(
          pending
            ? writing.current
              ? "Saving volume keys…"
              : "Sampling volume curve…"
            : draft
              ? "Release to save volume changes."
              : "Drag a point to adjust it. Hold Alt for fine control. Use Smooth changes for a gentle fade.",
        )}
      </p>
      <div className="gain-keyframe-actions">
        <Button
          disabled={
            pending || blocked() || !sampleScope || channel.keys.length < 2
          }
          onClick={() => {
            if (sampleScope)
              void submit(sampleScope, curveEdits(channel, true));
          }}
        >
          {t("Smooth changes")}
        </Button>
        <Button
          disabled={
            pending || blocked() || !sampleScope || channel.keys.length < 2
          }
          onClick={() => {
            if (sampleScope)
              void submit(sampleScope, curveEdits(channel, false));
          }}
        >
          {t("Straight changes")}
        </Button>
        <Button
          disabled={pending || blocked() || !sampleScope}
          onClick={() => addAtTime(localPlayhead(scene.playhead, element))}
        >
          {t("Add key at playhead")}
        </Button>
        <Button
          disabled={pending || blocked() || !sampleScope || !selected.size}
          onClick={() => {
            if (sampleScope)
              void submit(sampleScope, [
                {
                  kind: "delete",
                  keys: [...selected].map((id) => ({
                    channel_id: channel.id,
                    key_id: id,
                  })),
                },
              ]);
          }}
        >
          {t("Delete selected keys")}
        </Button>
        {draft && <Button onClick={cancelDrag}>{t("Cancel")}</Button>}
      </div>
      {error && <p role="alert">{t(error)}</p>}
    </section>
  );
}
