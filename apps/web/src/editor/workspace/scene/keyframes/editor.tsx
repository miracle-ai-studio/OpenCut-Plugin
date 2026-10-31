import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { ErrorNotice } from "../../../shared/error-notice";
import { Select } from "../../../shared/controls";
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useWorkspace } from "../../context";
import { t, useLocale, parameterLabel } from "../../../shared/locale";
import { editRecoveryBlocked } from "../edit-recovery";
import { BUILTIN_PRESETS, loadPresets, savePresets } from "./presets";
import type { CurvePreset } from "./presets";
import { channelRows, inputTime, refKey, seconds } from "./types";
import type {
  Channel,
  Curve,
  ExactTime,
  KeyframeEdit,
  KeyframeRequest,
  KeyRef,
  ScalarChannel,
} from "./types";
import {
  copySelection,
  pasteInto,
  pasteSelection,
  readClipboard,
  writeClipboard,
} from "./clipboard";
import "./keyframes.css";
import { LegacyKeyframeConversion } from "./legacy-conversion";

const properties = [
  "position_x_px",
  "position_y_px",
  "scale_x",
  "scale_y",
  "rotation_degrees",
  "opacity",
  "gain_decibels",
  "gain_linear_legacy",
  "font_size_classic_height90",
  "font_size_canvas_pixels",
  "letter_spacing_px",
  "line_height_multiplier",
  "text_background_padding_x_at_size15",
  "text_background_padding_y_at_size15",
  "text_background_offset_x_px",
  "text_background_offset_y_px",
  "text_background_corner_radius_percent",
  "graphic_stroke_width_px",
  "graphic_corner_radius_percent",
  "graphic_sides",
  "graphic_points",
  "graphic_depth_percent",
  "classic_blur_intensity",
  "legacy_blur_radius_px",
  "legacy_exposure_ev",
  "legacy_contrast",
  "legacy_saturation",
  "mask_center_x",
  "mask_center_y",
  "mask_width",
  "mask_height",
  "mask_rotation_degrees",
  "mask_scale",
  "mask_feather_classic_pixels",
  "mask_feather_legacy_canvas_fraction",
  "mask_stroke_width_px",
  "mask_font_size_classic_height90",
  "mask_font_size_canvas_pixels",
  "mask_letter_spacing_px",
  "mask_line_height_multiplier",
] as const;
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
type Gesture = {
  pointer: number;
  clientX: number;
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  width: number;
  span: number;
  keys: KeyRef[];
  delta: number;
  box: boolean;
  revision: string;
};

/** Input-only keyframe editing. Rust compiles requests from a fixed head and the
 * existing recovery coordinator owns the sole atomic write and history entry. */
export function KeyframeEditor({
  elementId,
  locked = false,
}: {
  elementId: string;
  locked?: boolean;
}) {
  useLocale();
  const { api, scene: editorState, controller, busy } = useWorkspace();
  const [available, setAvailable] = useState(false),
    [pending, setPending] = useState(false),
    [error, setError] = useState<string | null>(null),
    [selected, setSelected] = useState<Set<string>>(() => new Set()),
    [channelId, setChannelId] = useState("");
  const [localTime, setLocalTime] = useState("0"),
    [value, setValue] = useState("0"),
    [offset, setOffset] = useState("0"),
    [viewStart, setViewStart] = useState("0"),
    [viewSpan, setViewSpan] = useState("5"),
    [property, setProperty] =
      useState<(typeof properties)[number]>("position_x_px"),
    [target, setTarget] = useState("element");
  const [bindingKind, setBindingKind] = useState<
      "property" | "color_component" | "discrete"
    >("property"),
    [colorParameter, setColorParameter] =
      useState<
        Extract<
          ScalarChannel["binding"],
          { kind: "color_component" }
        >["parameter"]
      >("graphic_fill"),
    [component, setComponent] = useState<"red" | "green" | "blue" | "alpha">(
      "red",
    ),
    [discreteParameter, setDiscreteParameter] = useState<
      "graphic_stroke_align" | "mask_stroke_align"
    >("graphic_stroke_align"),
    [strokeAlign, setStrokeAlign] = useState<"inside" | "center" | "outside">(
      "center",
    );
  const [curve, setCurve] = useState<Curve>({ ...BUILTIN_PRESETS[0].curve }),
    [reference, setReference] = useState("1"),
    [custom, setCustom] = useState<CurvePreset[]>([]),
    [presetName, setPresetName] = useState(""),
    [draftGesture, setDraftGesture] = useState<Gesture | null>(null);
  const gesture = useRef<Gesture | null>(null),
    flight = useRef(false),
    epoch = useRef(0),
    attempt = useRef<KeyframeRequest | null>(null),
    mounted = useRef(true),
    curvePointer = useRef<{ id: number; control: 1 | 2 } | null>(null);
  const scene = editorState?.head.project.sequences.find((s) => s.id === editorState.sequenceId),
    track = scene?.tracks.find((t) =>
      t.elements.some((e) => e.id === elementId),
    ),
    element = track?.elements.find((e) => e.id === elementId),
    rows = scene ? channelRows(scene, elementId) : [],
    row = rows.find((r) => r.channel.id === channelId) ?? rows[0];
  const blocked = () => {
    const state = controller.getSnapshot(),
      view = state.scene;
    return (
      !view ||
      state.busy ||
      !!view.historical ||
      !!view.gainDraft ||
      editRecoveryBlocked(view.recovery) ||
      controller.scene.recovery.blocked() ||
      !!view.recovery?.record ||
      !!view.recovery?.error ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        view.audioPreview.phase,
      ) ||
      ["applying", "unconfirmed"].includes(
        view.timelineEdit.attempt?.phase ?? "",
      ) ||
      ["applying", "unconfirmed"].includes(view.clipboard.attempt?.phase ?? "")
    );
  };
  const disabled = locked || !!track?.locked || busy || pending || blocked();
  useEffect(() => {
    let live = true;
    setAvailable(false);
    if (api)
      void editorHostCapabilities(api)
        .then((c) => {
          if (live) setAvailable(c.tools.includes("opencut_keyframe_plan"));
        })
        .catch((e) => {
          if (live) setError(errorText(e));
        });
    return () => {
      live = false;
    };
  }, [api, api?.connectionIdentity]);
  useEffect(() => {
    mounted.current = true;
    try {
      setCustom(loadPresets());
    } catch (e) {
      setError(errorText(e));
    }
    return () => {
      mounted.current = false;
      epoch.current++;
      gesture.current = null;
    };
  }, []);
  useEffect(() => {
    epoch.current++;
    gesture.current = null;
    setDraftGesture(null);
    setSelected(new Set());
    setChannelId("");
    attempt.current = null;
    setError(null);
  }, [
    api,
    api?.connectionIdentity,
    editorState?.head.project.id,
    editorState?.head.revision,
    editorState?.sequenceId,
    elementId,
    editorState?.historical?.revision,
  ]);
  const choose = (ref: KeyRef, toggle: boolean) =>
    setSelected((old) => {
      const next = toggle ? new Set(old) : new Set<string>();
      const key = refKey(ref);
      if (toggle && next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const selectedRefs = (): KeyRef[] =>
    rows.flatMap((r) =>
      r.channel.keys
        .filter((k) =>
          selected.has(refKey({ channel_id: r.channel.id, key_id: k.id })),
        )
        .map((k) => ({ channel_id: r.channel.id, key_id: k.id })),
    );
  async function submit(edits: KeyframeEdit[], retry = false) {
    if (
      !mounted.current ||
      !api ||
      (!editorState || !scene) ||
      disabled ||
      flight.current ||
      controller.getSnapshot().scene?.selectedId !== elementId
    )
      return;
    const serial = epoch.current,
      sourceApi = api,
      sourceProject = editorState.head.project.id,
      sourceRevision = editorState.head.revision,
      sequenceId = editorState.sequenceId;
    const request = retry
      ? attempt.current
      : {
          project_id: sourceProject,
          expected_revision: sourceRevision,
          idempotency_key: crypto.randomUUID(),
          label: "Edit keyframes",
          sequence_id: sequenceId,
          edits,
        };
    if (!request) return;
    attempt.current = request;
    const fence = () => {
      const state = controller.getSnapshot();
      if (
        epoch.current !== serial ||
        state.api !== sourceApi ||
        state.scene?.head.project.id !== sourceProject ||
        state.scene.head.revision !== sourceRevision ||
        state.scene.sequenceId !== sequenceId ||
        state.scene.selectedId !== elementId ||
        blocked()
      )
        throw new Error(
          "The keyframe target changed. Resolve the existing request before making another edit.",
        );
    };
    flight.current = true;
    setPending(true);
    setError(null);
    try {
      fence();
      const transaction = await api.sdk.call(
        "opencut_keyframe_plan",
        request,
      );
      fence();
      if (
        transaction.project_id !== sourceProject ||
        transaction.expected_revision !== sourceRevision ||
        transaction.idempotency_key !== request.idempotency_key ||
        transaction.label !== request.label ||
        !transaction.operations.length ||
        transaction.operations.some(
          (op) => !["element_put", "track_put"].includes(op.type),
        )
      )
        throw new Error("The keyframe plan differs from the captured request.");
      await controller.scene.applyPlanned(transaction, sequenceId);
      if (serial === epoch.current) attempt.current = null;
    } catch (e) {
      if (mounted.current) setError(errorText(e));
    } finally {
      flight.current = false;
      if (mounted.current) setPending(false);
    }
  }
  const guarded = (work: () => void | Promise<void>) => {
    try {
      void Promise.resolve(work()).catch((e) => setError(errorText(e)));
    } catch (e) {
      setError(errorText(e));
    }
  };
  function addKey() {
    if (!element || !track) return;
    const time = inputTime(localTime),
      n = Number(value);
    if (!Number.isFinite(n) || !value.trim())
      throw new Error("Enter a finite keyframe value.");
    const key = {
      id: crypto.randomUUID(),
      time,
      value: n,
      left_handle: null,
      right_handle: null,
      segment_to_next: { kind: "linear" as const },
      tangent_mode: "broken" as const,
    };
    if (row) {
      if (row.channel.kind === "scalar")
        return submit([
          { kind: "put_scalar", channel_id: row.channel.id, key },
        ]);
      throw new Error("Use the discrete value control for this channel.");
    }
    return createChannel();
  }
  const [newChannel, setNewChannel] = useState(false);
  function createChannel() {
    if (!element || !track) return;
    const n = Number(value);
    if (!value.trim() || !Number.isFinite(n))
      throw new Error("Enter a finite keyframe value.");
    const scalar: ScalarChannel = {
      id: crypto.randomUUID(),
      kind: "scalar",
      target:
        target === "track"
          ? { kind: "track", track_id: track.id }
          : target.startsWith("mask:")
            ? { kind: "mask", element_id: elementId, mask_id: target.slice(5) }
            : target.startsWith("effect:")
              ? {
                  kind: "effect",
                  element_id: elementId,
                  effect_id: target.slice(7),
                }
              : { kind: "element", element_id: elementId },
      binding:
        bindingKind === "color_component"
          ? { kind: "color_component", parameter: colorParameter, component }
          : { kind: "property", parameter: property },
      keys: [
        {
          id: crypto.randomUUID(),
          time: inputTime(localTime),
          value: n,
          left_handle: null,
          right_handle: null,
          segment_to_next: { kind: "linear" },
          tangent_mode: "broken",
        },
      ],
      extrapolation: { before: "hold", after: "hold" },
    };
    const channel: Channel =
      bindingKind === "discrete"
        ? {
            id: scalar.id,
            kind: "discrete",
            target: scalar.target,
            parameter: discreteParameter,
            keys: [
              {
                id: scalar.keys[0].id,
                time: scalar.keys[0].time,
                value: { kind: "stroke_align", value: strokeAlign },
              },
            ],
          }
        : scalar;
    return submit([
      {
        kind: "channel_put",
        owner:
          target === "track"
            ? { kind: "track", track_id: track.id }
            : { kind: "element", element_id: elementId },
        channel,
      },
    ]);
  }
  function applyCurve() {
    if (!row || row.channel.kind !== "scalar") return;
    const keys = row.channel.keys.filter((k) =>
      selected.has(refKey({ channel_id: row.channel.id, key_id: k.id })),
    );
    if (keys.length !== 2)
      throw new Error("Select two adjacent scalar keys in this channel.");
    if (reference.trim() && !Number.isFinite(Number(reference)))
      throw new Error("Enter a finite flat-curve reference span.");
    return submit([
      {
        kind: "curve",
        channel_id: row.channel.id,
        left_key_id: keys[0].id,
        right_key_id: keys[1].id,
        curve,
        reference_span_value: reference.trim() ? Number(reference) : null,
      },
    ]);
  }
  function startPointer(e: ReactPointerEvent<SVGElement>, ref?: KeyRef) {
    if (disabled || (!editorState || !scene)) return;
    const svg =
      e.currentTarget.tagName.toLowerCase() === "svg"
        ? (e.currentTarget as SVGSVGElement)
        : e.currentTarget.ownerSVGElement;
    if (!svg) return;
    const rect = svg.getBoundingClientRect(),
      span = Number(viewSpan);
    if (!Number.isFinite(span) || span <= 0) return;
    const isSelected = ref && selected.has(refKey(ref));
    const keys = ref
      ? isSelected && !e.shiftKey && !e.metaKey && !e.ctrlKey
        ? selectedRefs()
        : [ref]
      : [];
    if (ref && !isSelected) choose(ref, e.shiftKey || e.metaKey || e.ctrlKey);
    else if (ref && (e.shiftKey || e.metaKey || e.ctrlKey)) {
      choose(ref, true);
      return;
    }
    svg.setPointerCapture(e.pointerId);
    const x = ((e.clientX - rect.left) * 1000) / rect.width,
      y = ((e.clientY - rect.top) * 200) / rect.height;
    const g: Gesture = {
      pointer: e.pointerId,
      clientX: e.clientX,
      startX: x,
      startY: y,
      endX: x,
      endY: y,
      width: rect.width * 0.92,
      span,
      keys,
      delta: 0,
      box: !ref,
      revision: editorState.head.revision,
    };
    gesture.current = g;
    setDraftGesture(g);
  }
  function movePointer(e: ReactPointerEvent<SVGSVGElement>) {
    const g = gesture.current;
    if (
      !g ||
      g.pointer !== e.pointerId ||
      disabled ||
      editorState?.head.revision !== g.revision
    )
      return;
    const rect = e.currentTarget.getBoundingClientRect();
    g.endX = ((e.clientX - rect.left) * 1000) / rect.width;
    g.endY = ((e.clientY - rect.top) * 200) / rect.height;
    g.delta = ((e.clientX - g.clientX) / g.width) * g.span;
    setDraftGesture({ ...g });
  }
  function endPointer(e: ReactPointerEvent<SVGSVGElement>) {
    const g = gesture.current;
    gesture.current = null;
    setDraftGesture(null);
    if (
      !g ||
      g.pointer !== e.pointerId ||
      disabled ||
      editorState?.head.revision !== g.revision
    )
      return;
    if (g.box) {
      const next = e.shiftKey ? new Set(selected) : new Set<string>();
      for (const [i, r] of visibleRows.entries())
        for (const k of r.channel.keys) {
          const x = xAt(k.time),
            y = 22 + i * 28;
          if (
            x >= Math.min(g.startX, g.endX) &&
            x <= Math.max(g.startX, g.endX) &&
            y >= Math.min(g.startY, g.endY) &&
            y <= Math.max(g.startY, g.endY)
          )
            next.add(refKey({ channel_id: r.channel.id, key_id: k.id }));
        }
      setSelected(next);
    } else if (Math.abs(e.clientX - g.clientX) > 2)
      guarded(() =>
        submit([
          {
            kind: "move",
            keys: g.keys,
            delta: inputTime(
              String(((e.clientX - g.clientX) / g.width) * g.span),
            ),
          },
        ]),
      );
  }
  const cancelGesture = () => {
    gesture.current = null;
    setDraftGesture(null);
    curvePointer.current = null;
  };
  const visibleRows = row
      ? [row, ...rows.filter((r) => r.channel.id !== row.channel.id)].slice(
          0,
          6,
        )
      : [],
    xAt = (time: ExactTime) =>
      40 + ((seconds(time) - Number(viewStart)) / Number(viewSpan)) * 920;
  const curvePath = `M 20 90 C ${20 + curve.x1 * 110} ${90 - curve.y1 * 60},${20 + curve.x2 * 110} ${90 - curve.y2 * 60},130 30`;
  const updateControl = (e: ReactPointerEvent<SVGSVGElement>) => {
    const point = curvePointer.current;
    if (!point || point.id !== e.pointerId) return;
    const r = e.currentTarget.getBoundingClientRect(),
      x = Math.max(
        0,
        Math.min(1, (((e.clientX - r.left) * 150) / r.width - 20) / 110),
      ),
      y = Math.max(
        -0.5,
        Math.min(1.5, (90 - ((e.clientY - r.top) * 120) / r.height) / 60),
      );
    setCurve((c) => ({
      ...c,
      [point.control === 1 ? "x1" : "x2"]: x,
      [point.control === 1 ? "y1" : "y2"]: y,
    }));
  };
  if (!available || !element) return null;
  return (
    <section
      className="editor-keyframes"
      aria-label={t("Keyframes")}
      onKeyDown={(e) => {
        if (e.key === "Escape") cancelGesture();
        if (
          (e.metaKey || e.ctrlKey) &&
          e.key.toLowerCase() === "a" &&
          e.target === e.currentTarget
        ) {
          e.preventDefault();
          setSelected(
            new Set(
              rows.flatMap((r) =>
                r.channel.keys.map((k) =>
                  refKey({ channel_id: r.channel.id, key_id: k.id }),
                ),
              ),
            ),
          );
        }
      }}
      tabIndex={0}
    >
      <h4>{t("Keyframes")}</h4>
      <p className="muted">
        {t(
          "Times are local to the element. Track channels use scene time. Dragging and pasting reject collisions and times outside the owner; no keys are silently replaced.",
        )}
      </p>
      {row?.channel.kind === "scalar" &&
        row.channel.keys.some(
          (key) => key.segment_to_next.kind === "legacy_normalized_bezier",
        ) && (
          <LegacyKeyframeConversion
            channel={row.channel}
            owner={row.owner}
            disabled={disabled}
            isBlocked={blocked}
            onChooseHold={(channel, owner) =>
              guarded(() => submit([{ kind: "channel_put", channel, owner }]))
            }
          />
        )}
      <fieldset disabled={disabled}>
        <label>
          {t("Animation channel")}
          <Select
            value={row?.channel.id ?? ""}
            onChange={(e) => setChannelId(e.target.value)}
          >
            {rows.map((r) => (
              <option key={r.channel.id} value={r.channel.id}>
                {r.label}
              </option>
            ))}
          </Select>
        </label>
        <button type="button" onClick={() => setNewChannel((v) => !v)}>
          {t("New animation channel")}
        </button>
      </fieldset>
      {(newChannel || !rows.length) && (
        <fieldset disabled={disabled}>
          <label>
            {t("Animation target")}
            <Select value={target} onChange={(e) => setTarget(e.target.value)}>
              <option value="element">{t("Element")}</option>
              <option value="track">{t("Track")}</option>
              {element.visual?.masks.map((mask, index) => (
                <option key={mask.id} value={`mask:${mask.id}`}>
                  {t("Mask")} {index + 1}
                </option>
              ))}
              {element.content.kind === "effect_layer" && (
                <option value={`effect:${element.content.effect.id}`}>
                  {t("Effect")}
                </option>
              )}
              {element.visual?.effects.map((effect, index) => (
                <option key={effect.id} value={`effect:${effect.id}`}>
                  {t("Effect")} {index + 1}
                </option>
              ))}
            </Select>
          </label>
          <label>
            {t("Channel value type")}
            <Select
              value={bindingKind}
              onChange={(e) =>
                setBindingKind(e.target.value as typeof bindingKind)
              }
            >
              <option value="property">{t("Number")}</option>
              <option value="color_component">
                {t("Color channel")}
              </option>
              <option value="discrete">{t("Stroke position")}</option>
            </Select>
          </label>
          {bindingKind === "color_component" && (
            <>
              <label>
                {t("Color parameter")}
                <Select
                  value={colorParameter}
                  onChange={(e) =>
                    setColorParameter(e.target.value as typeof colorParameter)
                  }
                >
                  {[
                    "text_color",
                    "text_background_color",
                    "graphic_fill",
                    "graphic_stroke",
                    "mask_stroke",
                  ].map((p) => (
                    <option key={p} value={p}>{parameterLabel(p)}</option>
                  ))}
                </Select>
              </label>
              <label>
                {t("Color component")}
                <Select
                  value={component}
                  onChange={(e) =>
                    setComponent(e.target.value as typeof component)
                  }
                >
                  {["red", "green", "blue", "alpha"].map((p) => (
                    <option key={p} value={p}>{parameterLabel(p)}</option>
                  ))}
                </Select>
              </label>
            </>
          )}
          {bindingKind === "discrete" && (
            <>
              <label>
                {t("Discrete parameter")}
                <Select
                  value={discreteParameter}
                  onChange={(e) =>
                    setDiscreteParameter(
                      e.target.value as typeof discreteParameter,
                    )
                  }
                >
                  <option value="graphic_stroke_align">
                    {t("Shape outline position")}
                  </option>
                  <option value="mask_stroke_align">
                    {t("Mask outline position")}
                  </option>
                </Select>
              </label>
              <label>
                {t("Stroke alignment")}
                <Select
                  value={strokeAlign}
                  onChange={(e) =>
                    setStrokeAlign(e.target.value as typeof strokeAlign)
                  }
                >
                  {["inside", "center", "outside"].map((p) => (
                    <option key={p} value={p}>{parameterLabel(p)}</option>
                  ))}
                </Select>
              </label>
            </>
          )}
          <label>
            {t("Animated parameter")}
            <Select
              value={property}
              disabled={bindingKind !== "property"}
              onChange={(e) => setProperty(e.target.value as typeof property)}
            >
              {properties.map((p) => (
                <option key={p} value={p}>{parameterLabel(p)}</option>
              ))}
            </Select>
          </label>
          <button type="button" onClick={() => guarded(createChannel)}>
            {t("Create channel with keyframe")}
          </button>
        </fieldset>
      )}
      <fieldset disabled={disabled}>
        <label>
          {t("Keyframe time (seconds)")}
          <input
            aria-label={t("Keyframe time (seconds)")}
            type="number"
            step="any"
            value={localTime}
            onChange={(e) => setLocalTime(e.target.value)}
          />
        </label>
        <label>
          {t("Keyframe value")}
          <input
            aria-label={t("Keyframe value")}
            type="number"
            step="any"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
        </label>
        <button type="button" onClick={() => guarded(addKey)}>
          {t("Add keyframe")}
        </button>
        <button
          type="button"
          onClick={() => {
            if (!row || row.channel.kind !== "scalar") return;
            // Keep the scalar narrowing inside the deferred edit.
            const channel = row.channel;
            guarded(() => {
              const n = Number(value);
              if (!value.trim() || !Number.isFinite(n))
                throw new Error("Enter a finite value.");
              return submit(
                channel.keys
                  .filter((k) =>
                    selected.has(
                      refKey({ channel_id: channel.id, key_id: k.id }),
                    ),
                  )
                  .map((key) => ({
                    kind: "put_scalar",
                    channel_id: channel.id,
                    key: { ...key, value: n },
                  })),
              );
            });
          }}
          disabled={!selected.size}
        >
          {t("Set selected values")}
        </button>
      </fieldset>
      {row?.channel.kind === "discrete" && (
        <fieldset disabled={disabled}>
          <label>
            {t("Discrete keyframe value")}
            <Select
              onChange={(e) =>
                guarded(() =>
                  submit([
                    {
                      kind: "put_discrete",
                      channel_id: row.channel.id,
                      key: {
                        id: crypto.randomUUID(),
                        time: inputTime(localTime),
                        value: {
                          kind: "stroke_align",
                          value: e.target.value as
                            "inside" | "center" | "outside",
                        },
                      },
                    },
                  ]),
                )
              }
              defaultValue=""
            >
              <option value="" disabled>
                {t("Choose stroke alignment")}
              </option>
              <option value="inside">{t("Inside")}</option>
              <option value="center">{t("Center")}</option>
              <option value="outside">{t("Outside")}</option>
            </Select>
          </label>
        </fieldset>
      )}
      <fieldset disabled={disabled}>
        <label>
          {t("View start (seconds)")}
          <input
            type="number"
            step="any"
            value={viewStart}
            onChange={(e) => {
              cancelGesture();
              setViewStart(e.target.value);
            }}
          />
        </label>
        <label>
          {t("View span (seconds)")}
          <input
            type="number"
            min="0.001"
            step="any"
            value={viewSpan}
            onChange={(e) => {
              cancelGesture();
              setViewSpan(e.target.value);
            }}
          />
        </label>
      </fieldset>
      <svg
        className="editor-keyframe-chart"
        viewBox="0 0 1000 200"
        aria-label={t("Keyframe timeline")}
        onPointerDown={(e) => startPointer(e)}
        onPointerMove={movePointer}
        onPointerUp={endPointer}
        onPointerCancel={cancelGesture}
        onLostPointerCapture={cancelGesture}
      >
        {visibleRows.map((r, index) => (
          <g key={r.channel.id}>
            <text x="4" y={17 + index * 28} fill="currentColor" fontSize="10">
              {r.label}
            </text>
            <line
              x1="40"
              x2="960"
              y1={22 + index * 28}
              y2={22 + index * 28}
              stroke="#555"
            />
            {r.channel.keys.map((key) => {
              const ref = { channel_id: r.channel.id, key_id: key.id },
                isSelected = selected.has(refKey(ref)),
                x =
                  xAt(key.time) +
                  (draftGesture &&
                  !draftGesture.box &&
                  draftGesture.keys.some((k) => refKey(k) === refKey(ref))
                    ? (draftGesture.delta / Number(viewSpan)) * 920
                    : 0),
                y = 22 + index * 28;
              return (
                <path
                  key={key.id}
                  className={`key ${isSelected ? "selected" : ""}`}
                  d={`M${x} ${y - 6}l6 6l-6 6l-6-6z`}
                  role="button"
                  aria-label={`${r.label} keyframe ${seconds(key.time)} seconds`}
                  aria-pressed={isSelected}
                  tabIndex={0}
                  onPointerDown={(e) => {
                    e.stopPropagation();
                    startPointer(e, ref);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === " " || e.key === "Enter") {
                      e.preventDefault();
                      choose(ref, e.shiftKey || e.metaKey || e.ctrlKey);
                    }
                  }}
                />
              );
            })}
          </g>
        ))}
        {draftGesture?.box && (
          <rect
            className="selection"
            x={Math.min(draftGesture.startX, draftGesture.endX)}
            y={Math.min(draftGesture.startY, draftGesture.endY)}
            width={Math.abs(draftGesture.startX - draftGesture.endX)}
            height={Math.abs(draftGesture.startY - draftGesture.endY)}
          />
        )}
      </svg>
      <div className="editor-keyframe-rows">
        {row?.channel.keys.map((key) => (
          <label key={key.id}>
            <input
              type="checkbox"
              disabled={disabled}
              checked={selected.has(
                refKey({ channel_id: row.channel.id, key_id: key.id }),
              )}
              onChange={() =>
                choose({ channel_id: row.channel.id, key_id: key.id }, true)
              }
            />
            {seconds(key.time)}s —{" "}
            {row.channel.kind === "scalar"
              ? String(key.value)
              : parameterLabel(typeof key.value === "object" && key.value !== null && "value" in key.value ? String(key.value.value) : String(key.value))}
          </label>
        ))}
      </div>
      <fieldset disabled={disabled}>
        <label>
          {t("Move selected by (seconds)")}
          <input
            type="number"
            step="any"
            value={offset}
            onChange={(e) => setOffset(e.target.value)}
          />
        </label>
        <button
          type="button"
          disabled={!selected.size}
          onClick={() =>
            guarded(() =>
              submit([
                {
                  kind: "move",
                  keys: selectedRefs(),
                  delta: inputTime(offset),
                },
              ]),
            )
          }
        >
          {t("Move selected keyframes")}
        </button>
        <button
          type="button"
          disabled={!selected.size}
          onClick={() =>
            guarded(() => submit([{ kind: "delete", keys: selectedRefs() }]))
          }
        >
          {t("Delete selected keyframes")}
        </button>
        <button
          type="button"
          disabled={!selected.size}
          onClick={() =>
            guarded(() => writeClipboard(copySelection(rows, selected)))
          }
        >
          {t("Copy keyframes")}
        </button>
        <button
          type="button"
          onClick={() =>
            guarded(async () => {
              const clip = await readClipboard();
              if (disabled) return;
              const at = inputTime(localTime);
              await submit(pasteSelection(clip, rows, elementId, at));
            })
          }
        >
          {t("Paste keyframes into element")}
        </button>
        <button
          type="button"
          disabled={!row}
          onClick={() =>
            guarded(async () => {
              const clip = await readClipboard();
              if (!row || disabled) return;
              if (clip.entries.length !== 1)
                throw new Error(
                  "Select a single clipboard channel for this destination.",
                );
              await submit([
                pasteInto(
                  clip.entries[0].channel,
                  row.channel,
                  inputTime(localTime),
                ),
              ]);
            })
          }
        >
          {t("Paste into selected channel")}
        </button>
      </fieldset>
      {row?.channel.kind === "scalar" && (
        <>
          <fieldset disabled={disabled}>
            <label>
              {t("Curve preset")}
              <Select
                defaultValue="smooth"
                onChange={(e) => {
                  const preset = [...BUILTIN_PRESETS, ...custom].find(
                    (p) => p.id === e.target.value,
                  );
                  if (preset) setCurve({ ...preset.curve });
                }}
              >
                {[...BUILTIN_PRESETS, ...custom].map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </Select>
            </label>
            {(["x1", "y1", "x2", "y2"] as const).map((key) => (
              <label key={key}>
                {key}
                <input
                  aria-label={`Curve ${key}`}
                  type="number"
                  step="any"
                  min={key.startsWith("x") ? 0 : -0.5}
                  max={key.startsWith("x") ? 1 : 1.5}
                  value={curve[key]}
                  onChange={(e) =>
                    setCurve((c) => ({ ...c, [key]: Number(e.target.value) }))
                  }
                />
              </label>
            ))}
            <label>
              {t("Flat curve reference span")}
              <input
                type="number"
                step="any"
                value={reference}
                onChange={(e) => setReference(e.target.value)}
              />
            </label>
            <button type="button" onClick={() => guarded(applyCurve)}>
              {t("Apply curve to selected segment")}
            </button>
          </fieldset>
          <svg
            className="editor-keyframe-curve"
            viewBox="0 0 150 120"
            aria-label={t("Bezier curve controls")}
            onPointerMove={updateControl}
            onPointerUp={() => {
              curvePointer.current = null;
            }}
            onPointerCancel={() => {
              curvePointer.current = null;
            }}
          >
            <line
              x1="20"
              y1="90"
              x2={20 + curve.x1 * 110}
              y2={90 - curve.y1 * 60}
              stroke="#888"
            />
            <line
              x1="130"
              y1="30"
              x2={20 + curve.x2 * 110}
              y2={90 - curve.y2 * 60}
              stroke="#888"
            />
            <path d={curvePath} fill="none" stroke="#7cb9ff" />
            {([1, 2] as const).map((control) => (
              <circle
                key={control}
                cx={20 + curve[control === 1 ? "x1" : "x2"] * 110}
                cy={90 - curve[control === 1 ? "y1" : "y2"] * 60}
                r="5"
                fill="#ddd"
                onPointerDown={(e) => {
                  if (disabled) return;
                  curvePointer.current = { id: e.pointerId, control };
                  e.currentTarget.ownerSVGElement?.setPointerCapture(
                    e.pointerId,
                  );
                }}
              />
            ))}
          </svg>
          <fieldset disabled={disabled}>
            <label>
              {t("Custom curve name")}
              <input
                maxLength={64}
                value={presetName}
                onChange={(e) => setPresetName(e.target.value)}
              />
            </label>
            <button
              type="button"
              onClick={() =>
                guarded(() => {
                  const next = [
                    ...custom,
                    {
                      id: crypto.randomUUID(),
                      name: presetName.trim(),
                      curve: { ...curve },
                    },
                  ];
                  savePresets(next);
                  setCustom(next);
                  setPresetName("");
                })
              }
            >
              {t("Save custom curve")}
            </button>
            <button
              type="button"
              disabled={!custom.length}
              onClick={() =>
                guarded(() => {
                  savePresets([]);
                  setCustom([]);
                })
              }
            >
              {t("Reset custom curves")}
            </button>
          </fieldset>
          <fieldset disabled={disabled}>
            {(["before", "after"] as const).map((side) => (
              <label key={side}>
                {t(
                  side === "before"
                    ? "Before first keyframe"
                    : "After last keyframe",
                )}
                <Select
                  value={
                    row.channel.kind === "scalar"
                      ? row.channel.extrapolation[side]
                      : "hold"
                  }
                  onChange={(e) =>
                    guarded(() =>
                      submit([
                        {
                          kind: "channel_put",
                          owner: row.owner,
                          channel: {
                            ...row.channel,
                            ...(row.channel.kind === "scalar"
                              ? {
                                  extrapolation: {
                                    ...row.channel.extrapolation,
                                    [side]: e.target.value as "hold" | "linear",
                                  },
                                }
                              : {}),
                          },
                        },
                      ]),
                    )
                  }
                >
                  <option value="hold">{t("Hold")}</option>
                  <option value="linear">{t("Linear")}</option>
                </Select>
              </label>
            ))}
          </fieldset>
        </>
      )}
      {error && (
        <ErrorNotice error={error} />
      )}
      {attempt.current && error && (
        <button
          type="button"
          disabled={disabled}
          onClick={() => void submit([], true)}
        >
          {t("Retry original keyframe request")}
        </button>
      )}
    </section>
  );
}
