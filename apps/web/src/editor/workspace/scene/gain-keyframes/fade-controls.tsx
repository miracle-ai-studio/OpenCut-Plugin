import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { usePropertyDraft } from "../property-edit/use-property-draft";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { useEditingWorkspace as useWorkspace } from "../../context";
import { ErrorNotice, localSessionExpired } from "../../../shared/error-notice";
import { t, useLocale } from "../../../shared/locale";
import { audioGainAvailability } from "../audio-gain";
import { same } from "../audio-gain-plan";
import { editRecoveryBlocked } from "../edit-recovery";
import type { ToolInput } from "../../../../lib/editor-sdk";
import { FadeInputError, fadeChannel, fadeInput, simpleFades } from "./fades";
import "./gain-keyframes.css";

export function GainFadeControls({ elementId }: { elementId: string }) {
  const { api, scene, controller } = useWorkspace();
  const root = useRef<HTMLDivElement>(null);
  const scope = `${api?.connectionIdentity}/${scene?.head.project.id}/${scene?.sequenceId}/${elementId}`;
  const keptDraft = useRef<{ scope: string; values: { fadeIn: string; fadeOut: string }; baseline: { fadeIn: string; fadeOut: string } } | null>(null);
  const restore = useRef<{
    field: "fadeIn" | "fadeOut";
    revision: string;
    project: string;
    sequence: string;
    element: string;
    identity: string | null;
    api: typeof api;
  } | null>(null);
  const restoreFocus = () => {
    const intent = restore.current,
      state = controller.getSnapshot(),
      view = state.scene;
    if (!intent) return;
    if (
      !root.current ||
      state.api !== intent.api ||
      state.api?.connectionIdentity !== intent.identity ||
      view?.head.project.id !== intent.project ||
      view.head.revision !== intent.revision ||
      view.sequenceId !== intent.sequence ||
      view.selectedId !== intent.element
    ) {
      restore.current = null;
      return;
    }
    const input = root.current.querySelector<HTMLInputElement>(
      `input[data-fade-field="${intent.field}"]`,
    );
    if (input && !input.disabled) {
      input.focus({ preventScroll: true });
      restore.current = null;
    }
  };
  useLayoutEffect(restoreFocus);
  return (
    <div ref={root}>
      <FadeForm
        key={`${api?.connectionIdentity}/${scene?.head.project.id}/${(scene?.historical ?? scene?.head)?.revision}/${scene?.sequenceId}/${elementId}`}
        elementId={elementId}
        preserved={keptDraft.current?.scope === scope ? keptDraft.current : null}
        onDraft={(values, baseline) => { keptDraft.current = { scope, values, baseline }; }}
        onReady={restoreFocus}
        onCommitted={(field, revision) => {
          if (!api || !scene) return;
          restore.current = {
            field,
            revision,
            project: scene.head.project.id,
            sequence: scene.sequenceId,
            element: elementId,
            identity: api.connectionIdentity,
            api,
          };
          restoreFocus();
        }}
      />
    </div>
  );
}

function FadeForm({
  elementId,
  preserved,
  onDraft,
  onCommitted,
  onReady,
}: {
  elementId: string;
  preserved: { values: { fadeIn: string; fadeOut: string }; baseline: { fadeIn: string; fadeOut: string } } | null;
  onDraft(values: { fadeIn: string; fadeOut: string }, baseline: { fadeIn: string; fadeOut: string }): void;
  onCommitted(field: "fadeIn" | "fadeOut", revision: string): void;
  onReady(): void;
}) {
  useLocale();
  const inputErrorId = useId();
  const { api, scene, controller, busy } = useWorkspace();
  const [captured] = useState(() => ({
    api,
    identity: api?.connectionIdentity,
    project: scene?.head.project.id,
    revision: (scene?.historical ?? scene?.head)?.revision,
    sequence: scene?.sequenceId,
    available: scene
      ? structuredClone(audioGainAvailability(scene, elementId))
      : null,
  }));
  const target = captured.available;
  const model = target ? simpleFades(target.element) : null;
  const [draft, setDraft] = useState(() => ({
    fadeIn: model ? preserved?.baseline.fadeIn === fadeInput(model.fadeIn) ? preserved.values.fadeIn : fadeInput(model.fadeIn) : "",
    fadeOut: model ? preserved?.baseline.fadeOut === fadeInput(model.fadeOut) ? preserved.values.fadeOut : fadeInput(model.fadeOut) : "",
  }));
  const dirty = !!model && (draft.fadeIn !== fadeInput(model.fadeIn) || draft.fadeOut !== fadeInput(model.fadeOut));
  const draftOwner = usePropertyDraft(controller, dirty);
  const changeDraft = (field: "fadeIn" | "fadeOut", value: string) => {
    const next = { ...draft, [field]: value }; setDraft(next);
    if (model) onDraft(next, { fadeIn: fadeInput(model.fadeIn), fadeOut: fadeInput(model.fadeOut) });
  };
  const [available, setAvailable] = useState<boolean | null>(null),
    [pending, setPending] = useState(false),
    [error, setError] = useState<unknown>(null);
  const alive = useRef(false),
    flight = useRef(false),
    abort = useRef<AbortController | null>(null);
  const reportError = (e: unknown) => {
    if (!alive.current) return;
    const current = controller.getSnapshot().api;
    if (
      localSessionExpired(e) &&
      current === captured.api &&
      current?.connectionIdentity === captured.identity
    )
      controller.report(e);
    else setError(e);
  };
  useEffect(() => {
    alive.current = true;
    if (captured.api) void editorHostCapabilities(captured.api)
      .then((c) => {
        if (alive.current)
          setAvailable(c.tools.includes("opencut_keyframe_plan"));
      })
      .catch(reportError);
    return () => {
      alive.current = false;
      abort.current?.abort();
    };
  }, [captured.api]);
  useEffect(() => {
    if (available && !pending) onReady();
  }, [available, pending, onReady]);
  const reason = () => {
    const s = controller.getSnapshot(),
      v = s.scene;
    if (v?.historical) return "Return to the current revision before editing.";
    if (
      !alive.current ||
      !target ||
      !v ||
      !s.api ||
      s.api !== captured.api ||
      s.api.connectionIdentity !== captured.identity ||
      v.head.project.id !== captured.project ||
      v.head.revision !== captured.revision ||
      v.sequenceId !== captured.sequence ||
      v.selectedId !== elementId
    )
      return "The audio target changed. Select the clip again.";
    const current = audioGainAvailability(v, elementId);
    if (
      !current ||
      !same(current.element, target.element) ||
      current.track.id !== target.track.id
    )
      return "The audio target changed. Select the clip again.";
    if (current.reason) return current.reason;
    if (v.externalRevision) return "The project changed in another client. Cancel the volume draft and refresh before saving.";
    if (
      s.busy ||
      v.propertyDrafts?.some(owner => owner !== draftOwner) ||
      v.historical ||
      v.gainDraft ||
      editRecoveryBlocked(v.recovery) ||
      controller.scene.recovery.blocked() ||
      v.recovery?.record ||
      v.recovery?.error ||
      ["reading", "staging", "applying"].includes(v.recovery?.phase ?? "") ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        v.audioPreview.phase,
      ) ||
      ["applying", "unconfirmed"].includes(
        v.timelineEdit.attempt?.phase ?? "",
      ) ||
      ["applying", "unconfirmed"].includes(v.clipboard.attempt?.phase ?? "") ||
      (v.audioGain && v.audioGain.phase !== "saved")
    )
      return "Pause playback and resolve the current edit before changing fades.";
    return null;
  };
  async function save(field: "fadeIn" | "fadeOut") {
    if (!model || !target || !captured.api || !available || flight.current)
      return;
    const blocked = reason();
    if (blocked) {
      setError(new Error(blocked));
      return;
    }
    setError(null);
    try {
      // A displayed rounded value is a view, not a request to quantize existing keys.
      if (draft[field] === fadeInput(model[field])) return;
      const channel = fadeChannel(target.element, model, field, draft[field]);
      if (!channel) return;
      const request: ToolInput<"opencut_keyframe_plan"> = {
        project_id: captured.project!,
        expected_revision: captured.revision!,
        sequence_id: captured.sequence!,
        idempotency_key: crypto.randomUUID(),
        label: "Edit audio fades",
        edits: [
          {
            kind: "channel_put",
            owner: { kind: "element", element_id: elementId },
            channel,
          },
        ],
      };
      const expected = structuredClone(target.element);
      const index = expected.channels.findIndex((c) => c.id === channel.id);
      if (index < 0) expected.channels.push(channel);
      else expected.channels[index] = channel;
      const cancel = new AbortController();
      abort.current = cancel;
      flight.current = true;
      setPending(true);
      const transaction = await captured.api.sdk.call(
        "opencut_keyframe_plan",
        request,
        { signal: cancel.signal },
      );
      if (!alive.current || cancel.signal.aborted) return;
      const changed = reason();
      if (changed) throw Error(changed);
      const op = transaction.operations[0];
      if (
        transaction.project_id !== request.project_id ||
        transaction.expected_revision !== request.expected_revision ||
        transaction.idempotency_key !== request.idempotency_key ||
        transaction.label !== request.label ||
        transaction.branch_name != null ||
        transaction.operations.length !== 1 ||
        op?.type !== "element_put" ||
        op.sequence_id !== request.sequence_id ||
        op.track_id !== target.track.id ||
        !same(op.element, expected)
      )
        throw Error(
          "The fade plan differs from the selected clip and original curve.",
        );
      const receipt = await controller.scene.applyPlanned(
        transaction,
        request.sequence_id,
        draftOwner,
      );
      if (receipt?.revision) onCommitted(field, receipt.revision);
    } catch (e) {
      if (alive.current && !abort.current?.signal.aborted) {
        reportError(e);
      }
    } finally {
      flight.current = false;
      if (alive.current) setPending(false);
    }
  }
  if (!target) return null;
  const blocked = reason();
  const inputError = error instanceof FadeInputError ? error : null;
  return (
    <section className="gain-fade-controls" aria-label={t("Audio fades")}>
      <h4>{t("Audio fades")}</h4>
      {!model ? (
        <p className="muted">
          {t(
            "These volume settings cannot be represented by two fade durations. Use the volume or curve controls below.",
          )}
        </p>
      ) : (
        <>
          {(["fadeIn", "fadeOut"] as const).map((field) => (
            <label key={field}>
              <span>{t(field === "fadeIn" ? "Fade in" : "Fade out")}</span>
              <span className="gain-fade-input">
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  max={Number(model.duration.ticks) / model.duration.timebase}
                  aria-label={t(
                    field === "fadeIn"
                      ? "Fade in (seconds)"
                      : "Fade out (seconds)",
                  )}
                  data-fade-field={field}
                  aria-invalid={inputError?.field === field || undefined}
                  aria-describedby={
                    inputError?.field === field ? inputErrorId : undefined
                  }
                  value={draft[field]}
                  disabled={!available || busy || pending || !!blocked}
                  onChange={(e) => {
                    const value = e.target.value;
                    changeDraft(field, value);
                    setError(null);
                  }}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Escape") {
                      e.preventDefault();
                      changeDraft(field, fadeInput(model[field]));
                      setError(null);
                    }
                    if (e.key === "Enter") {
                      e.preventDefault();
                      if (!e.repeat) void save(field);
                    }
                  }}
                />
                <span aria-hidden="true">s</span>
              </span>
            </label>
          ))}
          <p className="muted">
            {t("Enter to save this field. Esc to restore it.")}
          </p>
          {model.parameter === "gain_decibels" && (
            <p className="muted">
              {t("Decibel fades start or end at −60 dB; mute is separate.")}
            </p>
          )}
        </>
      )}
      {model && blocked && <p className="muted">{t(blocked)}</p>}
      {model && available === null && !error && (
        <p className="muted">{t("Checking fade editing availability…")}</p>
      )}
      {model && available === false && (
        <p className="muted">
          {t("Fade editing is unavailable on this host.")}
        </p>
      )}
      {dirty && !pending && <button type="button" onClick={() => {
        if (!model) return;
        const next = { fadeIn: fadeInput(model.fadeIn), fadeOut: fadeInput(model.fadeOut) };
        setDraft(next); onDraft(next, next); setError(null);
      }}>{t("Cancel fade draft")}</button>}
      {pending && <p role="status">{t("Saving audio fades…")}</p>}
      {inputError ? (
        <p id={inputErrorId} className="gain-fade-error" role="alert">
          {t(inputError.message)}
        </p>
      ) : (
        error != null && <ErrorNotice error={error} />
      )}
    </section>
  );
}
