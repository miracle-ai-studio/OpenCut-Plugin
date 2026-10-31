import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { usePropertyDraft } from "./use-property-draft";
import { convertGainUnit } from "./volume";
import { RateInput } from "../../../shared/rate-input";
import { ErrorNotice } from "../../../shared/error-notice";
import { Select } from "../../../shared/controls";
import { useEffect, useRef, useState } from "react";
import { useWorkspace } from "../../context";
import { t, useLocale } from "../../../shared/locale";
import { editRecoveryBlocked } from "../edit-recovery";
import type { ToolInput } from "../../../../lib/editor-sdk";
import type { Element } from "../types";
type Request = ToolInput<"opencut_element_properties_plan">;
const transformFields = [
  "position_x_px",
  "position_y_px",
  "scale_x",
  "scale_y",
  "rotation_degrees",
] as const;
const labels = {
  position_x_px: "Horizontal position (pixels)",
  position_y_px: "Vertical position (pixels)",
  scale_x: "Horizontal scale",
  scale_y: "Vertical scale",
  rotation_degrees: "Rotation (degrees)",
};
function audioOf(element: Element) {
  return element.content.kind === "video" || element.content.kind === "audio"
    ? element.content.audio
    : null;
}
function retimeOf(element: Element) {
  return element.content.kind === "video" || element.content.kind === "audio"
    ? element.content.retime
    : null;
}
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const numeric = (value: string) => {
  const n = Number(value);
  if (!value.trim() || !Number.isFinite(n))
    throw new Error("Enter a finite property value.");
  return n;
};
const integer = (value: string) => {
  if (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 4294967295)
    throw new Error("Enter a positive 32-bit integer.");
  return Number(value);
};
export function ElementPropertiesEditor({
  elementId,
  locked = false,
  showAudio = true,
}: {
  elementId: string;
  locked?: boolean;
  showAudio?: boolean;
}) {
  const { api, scene } = useWorkspace();
  return (
    <PropertiesForm
      key={`${api?.connectionIdentity}/${scene?.head.project.id}/${scene?.head.revision}/${scene?.sequenceId}/${elementId}`}
      elementId={elementId}
      locked={locked}
      showAudio={showAudio}
    />
  );
}
function PropertiesForm({
  elementId,
  locked,
  showAudio,
}: {
  elementId: string;
  locked: boolean;
  showAudio: boolean;
}) {
  useLocale();
  const { api, scene, controller, busy } = useWorkspace();
  const [captured] = useState(() => {
    const state = controller.getSnapshot(),
      view = state.scene,
      track = view?.head.project.sequences
        .find((s) => s.id === view.sequenceId)
        ?.tracks.find((t) => t.elements.some((e) => e.id === elementId));
    return {
      api: state.api,
      projectId: view?.head.project.id,
      revision: view?.head.revision,
      sequenceId: view?.sequenceId,
      trackId: track?.id,
      element: structuredClone(track?.elements.find((e) => e.id === elementId)),
    };
  });
  const element = captured.element,
    audio = element && showAudio ? audioOf(element) : null,
    retime = element ? retimeOf(element) : null,
    visual = element?.visual;
  const [transform, setTransform] = useState(
      () =>
        Object.fromEntries(
          transformFields.map((k) => [k, String(visual?.transform[k] ?? 0)]),
        ) as Record<(typeof transformFields)[number], string>,
    ),
    [opacity, setOpacity] = useState(String(visual?.opacity ?? 1)),
    [gain, setGain] = useState(String(audio?.gain.value ?? 0)),
    [gainUnit, setGainUnit] = useState(audio?.gain.unit ?? "decibels"),
    [muted, setMuted] = useState(audio?.muted ?? false),
    [numerator, setNumerator] = useState(String(retime?.rate.numerator ?? 1)),
    [denominator, setDenominator] = useState(
      String(retime?.rate.denominator ?? 1),
    ),
    [pitch, setPitch] = useState(retime?.maintain_pitch ?? false),
    [rateReset, setRateReset] = useState(0),
    [available, setAvailable] = useState(false),
    [pending, setPending] = useState(false),
    [error, setError] = useState<string | null>(null);
  const dirty = !!element && (!!visual && (transformFields.some(k => transform[k] !== String(visual.transform[k])) || opacity !== String(visual.opacity)) ||
    !!audio && (gain !== String(audio.gain.value) || gainUnit !== audio.gain.unit || muted !== audio.muted) ||
    !!retime && (numerator !== String(retime.rate.numerator) || denominator !== String(retime.rate.denominator) || pitch !== retime.maintain_pitch));
  const draftOwner = usePropertyDraft(controller, dirty);
  const gainAnimated = element?.channels.some(c => c.kind === "scalar" && c.binding.kind === "property" && ["gain_decibels", "gain_linear_legacy"].includes(c.binding.parameter) && c.keys.length > 0);
  const flight = useRef(false),
    alive = useRef(true),
    attempt = useRef<Request | null>(null);
  useEffect(() => {
    alive.current = true;
    let live = true;
    if (api)
      void editorHostCapabilities(api)
        .then((c) => {
          if (live)
            setAvailable(
              c.tools.includes("opencut_element_properties_plan"),
            );
        })
        .catch((e) => {
          if (live) setError(message(e));
        });
    return () => {
      live = false;
      alive.current = false;
    };
  }, [api]);
  const reason = () => {
    const state = controller.getSnapshot(),
      view = state.scene;
    if (
      !alive.current ||
      state.api !== captured.api ||
      !view ||
      view.externalRevision ||
      view.head.project.id !== captured.projectId ||
      view.head.revision !== captured.revision ||
      view.sequenceId !== captured.sequenceId ||
      view.selectedId !== elementId
    )
      throw new Error("The property target changed. Reopen its controls.");
    const track = view.head.project.sequences
        .find((s) => s.id === view.sequenceId)
        ?.tracks.find((t) => t.id === captured.trackId),
      current = track?.elements.find((e) => e.id === elementId);
    if (
      !track ||
      track.locked ||
      !current ||
      JSON.stringify(current) !== JSON.stringify(element)
    )
      throw new Error("The property target changed or its track is locked.");
    if (
      state.busy ||
      view.propertyDrafts?.some(owner => owner !== draftOwner) ||
      view.historical ||
      view.gainDraft ||
      view.recovery?.record ||
      view.recovery?.error ||
      editRecoveryBlocked(view.recovery) ||
      controller.scene.recovery.blocked() ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        view.audioPreview.phase,
      ) ||
      ["applying", "unconfirmed"].includes(
        view.timelineEdit.attempt?.phase ?? "",
      ) ||
      ["applying", "unconfirmed"].includes(view.clipboard.attempt?.phase ?? "")
    )
      throw new Error(
        "Finish playback or resolve the existing edit before changing properties.",
      );
  };
  const blocked = () => {
    try {
      reason();
      return false;
    } catch {
      return true;
    }
  };
  const disabled = locked || busy || pending || blocked();
  async function save(retry = false) {
    if (!element || !api || !scene || flight.current || disabled) return;
    if (!dirty && !retry) return;
    setError(null);
    flight.current = true;
    setPending(true);
    try {
      reason();
      let request = retry ? attempt.current : null;
      if (!request) {
        const changedTransform =
            visual &&
            transformFields.some(
              (k) => transform[k] !== String(visual.transform[k]),
            ),
          changedAudio =
            audio &&
            (gain !== String(audio.gain.value) || gainUnit !== audio.gain.unit),
          changedRetime =
            retime &&
            (numerator !== String(retime.rate.numerator) ||
              denominator !== String(retime.rate.denominator) ||
              pitch !== retime.maintain_pitch);
        request = {
          project_id: captured.projectId!,
          expected_revision: captured.revision!,
          sequence_id: captured.sequenceId!,
          element_id: elementId,
          idempotency_key: crypto.randomUUID(),
          label: "Edit element properties",
          transform: changedTransform
            ? {
                ...visual!.transform,
                ...Object.fromEntries(
                  transformFields
                    .filter(
                      (k) => transform[k] !== String(visual!.transform[k]),
                    )
                    .map((k) => [k, numeric(transform[k])]),
                ),
              }
            : null,
          opacity:
            visual && opacity !== String(visual.opacity)
              ? numeric(opacity)
              : null,
          audio_gain: changedAudio
            ? { unit: gainUnit, value: numeric(gain) }
            : null,
          muted: audio && muted !== audio.muted ? muted : null,
          retime: changedRetime
            ? {
                ...retime!,
                rate: {
                  numerator: integer(numerator),
                  denominator: integer(denominator),
                },
                maintain_pitch: pitch,
              }
            : null,
        };
        attempt.current = request;
      }
      const transaction = await api.sdk.call(
        "opencut_element_properties_plan",
        request,
      );
      reason();
      if (
        transaction.project_id !== request.project_id ||
        transaction.expected_revision !== request.expected_revision ||
        transaction.idempotency_key !== request.idempotency_key ||
        transaction.operations.length !== 1 ||
        transaction.operations[0].type !== "element_put" ||
        transaction.operations[0].element.id !== elementId ||
        transaction.operations[0].track_id !== captured.trackId
      )
        throw new Error("The property plan differs from the captured target.");
      await controller.scene.applyPlanned(transaction, captured.sequenceId!, draftOwner);
      attempt.current = null;
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      flight.current = false;
      if (alive.current) setPending(false);
    }
  }
  if (!available || !element || (!visual && !audio && !retime)) return null;
  return (
    <section
      className="v4-element-properties"
      aria-label={t("Element properties")}
    >
      <h4>{t("Element properties")}</h4>
      {element.channels.length > 0 && (
        <p className="muted">
          {t(
            "Animation channels are preserved. Animated parameters may override the base values shown here.",
          )}
        </p>
      )}
      <fieldset disabled={disabled}>
        {visual && (
          <>
            {transformFields.map((k) => (
              <label key={k}>
                {t(labels[k])}
                <input
                  type="number"
                  step="any"
                  aria-label={t(labels[k])}
                  value={transform[k]}
                  onChange={(e) =>
                    setTransform((v) => ({ ...v, [k]: e.target.value }))
                  }
                />
              </label>
            ))}
            <label>
              {t("Opacity")}
              <input
                type="number"
                step="any"
                min="0"
                max="1"
                value={opacity}
                onChange={(e) => setOpacity(e.target.value)}
              />
            </label>
          </>
        )}
        {audio && (
          <>
            <label>
              {t("Audio gain unit")}
              <Select
                value={gainUnit}
                disabled={gainAnimated}
                onChange={(e) => { try { const unit = e.target.value as typeof gainUnit; setGain(convertGainUnit(gain, gainUnit, unit)); setGainUnit(unit); setError(null); } catch (failure) { setError(message(failure)); } }}
              >
                <option value="decibels">dB</option>
                <option value="linear_legacy">{t("Volume multiplier")}</option>
              </Select>
            </label>
            <label>
              {t("Audio gain")}
              <input
                type="number"
                step="any"
                value={gain}
                onChange={(e) => setGain(e.target.value)}
              />
            </label>
            <label>
              <input
                type="checkbox"
                checked={muted}
                onChange={(e) => setMuted(e.target.checked)}
              />
              {t("Mute element")}
            </label>
          </>
        )}
        {retime && (
          <>
            <RateInput key={rateReset} numerator={numerator} denominator={denominator} onChange={(n,d) => {setNumerator(n);setDenominator(d);}} />
            <label>
              <input
                type="checkbox"
                checked={pitch}
                onChange={(e) => setPitch(e.target.checked)}
              />
              {t("Maintain audio pitch")}
            </label>
            <p className="muted">
              {t(
                "Changing speed adjusts the clip duration while preserving its source range. Animation beyond the new end is trimmed. Overlapping clips prevent the change from being saved.",
              )}
            </p>
          </>
        )}
        <button type="button" onClick={() => void save()}>
          {t("Save element properties")}
        </button>
      </fieldset>
      {dirty && !pending && <button type="button" onClick={() => {
        setTransform(Object.fromEntries(transformFields.map(k => [k, String(visual?.transform[k] ?? 0)])) as typeof transform);
        setOpacity(String(visual?.opacity ?? 1)); setGain(String(audio?.gain.value ?? 0)); setGainUnit(audio?.gain.unit ?? "decibels");
        setMuted(audio?.muted ?? false); setNumerator(String(retime?.rate.numerator ?? 1)); setDenominator(String(retime?.rate.denominator ?? 1)); setPitch(retime?.maintain_pitch ?? false); setRateReset(value => value + 1); setError(null);
      }}>{t("Cancel property draft")}</button>}
      {error && <ErrorNotice error={error} />}
      {error && attempt.current && (
        <button
          type="button"
          disabled={disabled}
          onClick={() => void save(true)}
        >
          {t("Retry original property request")}
        </button>
      )}
    </section>
  );
}
