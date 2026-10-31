import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useEditingWorkspace } from "../../context";
import { ErrorNotice, localSessionExpired } from "../../../shared/error-notice";
import { t, useLocale } from "../../../shared/locale";
import { editRecoveryBlocked } from "../edit-recovery";
import { same } from "../audio-gain-plan";
import type { ToolInput } from "../../../../lib/editor-sdk";
import type { SessionState } from "../types";
import { usePropertyDraft } from "./use-property-draft";
import {
  VolumeInputError,
  authoredVolume,
  displayVolume,
  parseVolume,
  volumeLevel,
  volumeTargets,
} from "./volume";

type Field = "percent" | "db";
type FocusField = Field | "muted";
type Draft = { field: Field; value: string };
const selection = (scene: SessionState) =>
  JSON.stringify([scene.selectedId, scene.timelineEdit.selection]);

export function VolumeControls({
  elementIds,
}: {
  elementIds: readonly string[];
}) {
  const { api, scene, controller } = useEditingWorkspace();
  const root = useRef<HTMLDivElement>(null),
    focus = useRef<{
      field: FocusField;
      revision: string;
      scope: string;
      api: typeof api;
    } | null>(null);
  const scope = JSON.stringify([
    api?.connectionIdentity,
    scene?.head.project.id,
    scene?.sequenceId,
    elementIds,
  ]);
  const restore = () => {
    const intent = focus.current;
    if (!intent) return;
    const current = controller.getSnapshot(), view = current.scene;
    const ids = view?.timelineEdit.selection.length ? view.timelineEdit.selection.map(target => target.element_id) : view?.selectedId ? [view.selectedId] : [];
    const liveScope = JSON.stringify([current.api?.connectionIdentity, view?.head.project.id, view?.sequenceId, ids]);
    if (intent.api !== current.api || intent.scope !== liveScope || intent.revision !== view?.head.revision) {
      focus.current = null;
      return;
    }
    const input = root.current?.querySelector<HTMLInputElement>(
      `[data-volume-field="${intent.field}"]`,
    );
    if (input && !input.disabled) {
      input.focus({ preventScroll: true });
      focus.current = null;
    }
  };
  useLayoutEffect(restore);
  return (
    <div ref={root}>
      <VolumeForm
        key={`${scope}/${(scene?.historical ?? scene?.head)?.revision}`}
        elementIds={elementIds}
        onReady={restore}
        onCommitted={(field, revision) => {
          focus.current = { field, revision, scope, api };
          restore();
        }}
      />
    </div>
  );
}

function VolumeForm({
  elementIds,
  onReady,
  onCommitted,
}: {
  elementIds: readonly string[];
  onReady(): void;
  onCommitted(field: FocusField, revision: string): void;
}) {
  useLocale();
  const { api, controller, scene, busy } = useEditingWorkspace();
  const [captured] = useState(() => ({
    api,
    identity: api?.connectionIdentity,
    project: scene?.head.project.id,
    revision: (scene?.historical ?? scene?.head)?.revision,
    sequence: scene?.sequenceId,
    selection: scene ? selection(scene) : "",
    targets: scene ? structuredClone(volumeTargets(scene, elementIds)) : [],
  }));
  const targets = captured.targets.filter((target) => target !== null);
  const complete = targets.length === elementIds.length && targets.length > 0;
  const levels = targets.map(volumeLevel),
    common =
      complete &&
      levels[0] !== null &&
      levels.every((level) => level === levels[0])
        ? levels[0]
        : null;
  const custom = levels.some((level) => level === null);
  const maxPercent = targets.some(
    (target) => target.audio.gain.unit === "linear_legacy",
  )
    ? 400
    : 1000;
  const original = {
    percent: common === null ? "" : displayVolume(common * 100),
    db:
      common === null || common === 0
        ? ""
        : displayVolume(20 * Math.log10(common)),
  };
  const [draft, setDraft] = useState<Draft | null>(null),
    [pending, setPending] = useState(false);
  const [capabilities, setCapabilities] = useState<readonly string[] | null>(
      null,
    ),
    [error, setError] = useState<unknown>(null);
  const dirty = draft !== null && draft.value !== original[draft.field];
  const owner = usePropertyDraft(controller, dirty);
  const latestDraft = useRef(draft);
  latestDraft.current = draft;
  const alive = useRef(false),
    flight = useRef(false),
    cancel = useRef<AbortController | null>(null);
  const pointer = useRef<number | null>(null),
    key = useRef<string | null>(null);
  const report = (failure: unknown) => {
    if (!alive.current) return;
    if (
      localSessionExpired(failure) &&
      controller.getSnapshot().api === captured.api &&
      captured.api?.connectionIdentity === captured.identity
    )
      controller.report(failure);
    else setError(failure);
  };
  useEffect(() => {
    alive.current = true;
    if (captured.api) void editorHostCapabilities(captured.api)
      .then((value) => {
        if (alive.current) setCapabilities(value.tools);
      })
      .catch(report);
    return () => {
      alive.current = false;
      cancel.current?.abort();
    };
  }, [captured.api]);
  useLayoutEffect(() => {
    if (capabilities && !pending) onReady();
  });
  const reason = () => {
    const state = controller.getSnapshot(),
      view = state.scene;
    if (view?.historical)
      return "Return to the current revision before editing.";
    if (
      !alive.current ||
      !view ||
      !complete ||
      state.api !== captured.api ||
      state.api?.connectionIdentity !== captured.identity ||
      view?.head.project.id !== captured.project ||
      view.head.revision !== captured.revision ||
      view.sequenceId !== captured.sequence ||
      selection(view) !== captured.selection
    )
      return "The audio target changed. Select the clip again.";
    if (view.externalRevision)
      return "The project changed in another client. Cancel the volume draft and refresh before saving.";
    const now = volumeTargets(view, elementIds);
    for (let i = 0; i < now.length; i++) {
      const target = now[i],
        before = captured.targets[i];
      if (
        !target ||
        !before ||
        target.track.id !== before.track.id ||
        !same(target.element, before.element)
      )
        return "The audio target changed. Select the clip again.";
      if (target.reason) return target.reason;
    }
    if (
      state.busy ||
      view.gainDraft ||
      view.propertyDrafts?.some((id) => id !== owner) ||
      editRecoveryBlocked(view.recovery) ||
      controller.scene.recovery.blocked() ||
      (view.audioGain && view.audioGain.phase !== "saved") ||
      (view.sourceAudio && view.sourceAudio.phase !== "saved") ||
      view.timelineEdit.attempt ||
      view.clipboard.attempt ||
      view.media.attempt ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        view.audioPreview.phase,
      )
    )
      return "Pause playback and confirm or cancel other edits before changing volume.";
    return null;
  };
  // Lifecycle setup is not a target change. Keep the save-time guard strict,
  // but present capability loading separately until its first result arrives.
  const loading = capabilities === null && !error;
  const blocked = capabilities === null ? null : reason();
  const available =
    !!capabilities &&
    targets.every((target) =>
      capabilities.includes(
        target.animated
          ? "opencut_keyframe_plan"
          : "opencut_element_properties_plan",
      ),
    );
  const disabled = !available || busy || pending || !!blocked || custom;
  const reset = () => {
    pointer.current = null;
    key.current = null;
    latestDraft.current = null;
    setDraft(null);
    setError(null);
  };
  useEffect(() => {
    const blur = () => {
      if (pointer.current !== null || key.current !== null) reset();
    };
    window.addEventListener("blur", blur);
    return () => window.removeEventListener("blur", blur);
  }, []);
  const update = (next: Draft) => {
    latestDraft.current = next;
    setDraft(next);
    setError(null);
  };
  async function save(
    value: Draft | null = latestDraft.current,
    muted?: boolean,
  ) {
    if (
      muted === undefined &&
      (!value || flight.current || value.value === original[value.field])
    ) {
      if (!flight.current) reset();
      return;
    }
    const denied = reason();
    if (
      flight.current ||
      denied ||
      (muted === undefined
        ? !available || custom
        : !capabilities?.includes("opencut_element_properties_plan"))
    ) {
      setError(
        Error(denied ?? "Volume editing is unavailable for this selection."),
      );
      return;
    }
    const abort = new AbortController();
    cancel.current = abort;
    const id = crypto.randomUUID(),
      label = "Change clip volume";
    if (muted !== undefined) controller.scene.propertyDraft(owner, true);
    flight.current = true;
    setPending(true);
    setError(null);
    try {
      const amplitude =
        muted === undefined ? parseVolume(value!.field, value!.value) : null;
      const authored = targets.map((target) => {
        if (muted === undefined) return authoredVolume(target, amplitude!);
        const expected = structuredClone(target.element);
        if (
          expected.content.kind === "audio" ||
          expected.content.kind === "video"
        )
          expected.content.audio.muted = muted;
        return { expected, channel: null };
      });
      const operations: ToolInput<"opencut_edit_apply">["operations"] = [];
      for (let i = 0; i < targets.length; i++) {
        abort.signal.throwIfAborted();
        const target = targets[i],
          change = authored[i];
        if (same(change.expected, target.element)) continue;
        const base = {
          project_id: captured.project!,
          expected_revision: captured.revision!,
          sequence_id: captured.sequence!,
          idempotency_key: id,
          label,
        };
        const plan = change.channel
          ? await captured.api!.sdk.call(
              "opencut_keyframe_plan",
              {
                ...base,
                edits: [
                  {
                    kind: "channel_put",
                    owner: { kind: "element", element_id: target.element.id },
                    channel: change.channel,
                  },
                ],
              },
              { signal: abort.signal },
            )
          : await captured.api!.sdk.call(
              "opencut_element_properties_plan",
              {
                ...base,
                element_id: target.element.id,
                transform: null,
                opacity: null,
                muted: muted ?? null,
                retime: null,
                audio_gain:
                  muted === undefined &&
                  (change.expected.content.kind === "audio" ||
                    change.expected.content.kind === "video")
                    ? change.expected.content.audio.gain
                    : null,
              },
              { signal: abort.signal },
            );
        abort.signal.throwIfAborted();
        const changed = reason();
        if (changed) throw Error(changed);
        const op = plan.operations[0];
        if (
          plan.project_id !== base.project_id ||
          plan.expected_revision !== base.expected_revision ||
          plan.idempotency_key !== id ||
          plan.label !== label ||
          plan.branch_name != null ||
          plan.operations.length !== 1 ||
          op?.type !== "element_put" ||
          op.sequence_id !== base.sequence_id ||
          op.track_id !== target.track.id ||
          !same(op.element, change.expected)
        )
          throw Error(
            "The volume plan differs from the captured clips and original curves.",
          );
        operations.push(op);
      }
      if (!operations.length) {
        reset();
        return;
      }
      if (
        new Set(
          operations.map((op) =>
            op.type === "element_put" ? op.element.id : "",
          ),
        ).size !== operations.length
      )
        throw Error("The volume plan contains duplicate targets.");
      const denied = reason();
      if (denied) throw Error(denied);
      const receipt = await controller.scene.applyPlanned(
        {
          project_id: captured.project!,
          expected_revision: captured.revision!,
          idempotency_key: id,
          label,
          operations,
        },
        captured.sequence!,
        owner,
      );
      if (receipt?.revision)
        onCommitted(
          muted !== undefined ? "muted" : value!.field,
          receipt.revision,
        );
    } catch (failure) {
      if (!abort.signal.aborted) report(failure);
    } finally {
      flight.current = false;
      if (muted !== undefined) controller.scene.propertyDraft(owner, false);
      if (alive.current) setPending(false);
    }
  }
  if (!complete) return null;
  let amplitude = common, invalidDraft = false;
  if (draft) {
    try {
      amplitude = parseVolume(draft.field, draft.value);
    } catch {
      amplitude = null; invalidDraft = true;
    }
  }
  invalidDraft ||= !!draft && (error instanceof VolumeInputError || amplitude !== null && amplitude * 100 > maxPercent);
  const values = {
    percent:
      draft?.field === "percent"
        ? draft.value
        : amplitude === null
          ? ""
          : displayVolume(amplitude * 100),
    db:
      draft?.field === "db"
        ? draft.value
        : amplitude === null || amplitude === 0
          ? ""
          : displayVolume(20 * Math.log10(amplitude)),
  };
  const pointerValue = (node: HTMLDivElement, x: number) => {
    const rect = node.getBoundingClientRect();
    return {
      field: "percent" as const,
      value: displayVolume(
        Math.max(0, Math.min(1, (x - rect.left) / rect.width)) * maxPercent,
      ),
    };
  };
  return (
    <section
      className="workbench-volume-controls"
      aria-label={t("Volume")}
      aria-busy={loading}
      data-shortcuts-ignore
    >
      <h4>{t("Volume")}</h4>
      <label className="workbench-volume-mute">
        <input
          type="checkbox"
          ref={(node) => {
            if (node)
              node.indeterminate = targets.some(
                (target) => target.audio.muted !== targets[0].audio.muted,
              );
          }}
          data-volume-field="muted"
          aria-label={t("Mute selected clips")}
          checked={targets.every((target) => target.audio.muted)}
          disabled={
            busy ||
            pending ||
            !!blocked ||
            dirty ||
            !capabilities?.includes("opencut_element_properties_plan")
          }
          onChange={(event) => void save(null, event.target.checked)}
        />
        {t("Mute selected clips")}
      </label>
      <div className="workbench-volume-inputs">
        {(["percent", "db"] as const).map((field) => (
          <label key={field}>
            <span>
              {field === "percent"
                ? t("Volume (percent)")
                : t("Volume (decibels)")}
            </span>
            <span>
              <input
                type="text"
                inputMode="decimal"
                aria-label={t(
                  field === "percent"
                    ? "Volume (percent)"
                    : "Volume (decibels)",
                )}
                data-volume-field={field}
                aria-invalid={error instanceof VolumeInputError && draft?.field === field || undefined}
                aria-describedby={error instanceof VolumeInputError ? `${owner}-error` : undefined}
                value={values[field]}
                placeholder={common === 0 && field === "db" ? "−∞" : "—"}
                disabled={disabled}
                onChange={(event) =>
                  update({ field, value: event.target.value })
                }
                onKeyDown={(event) => {
                  event.stopPropagation();
                  if (event.key === "Escape") {
                    event.preventDefault();
                    reset();
                  }
                  if (event.key === "Enter") {
                    event.preventDefault();
                    if (!event.repeat) void save();
                  }
                }}
              />
              <span aria-hidden="true">{field === "percent" ? "%" : "dB"}</span>
            </span>
          </label>
        ))}
      </div>
      <div
        className="workbench-volume-slider"
        role="slider"
        tabIndex={disabled ? -1 : 0}
        aria-label={t("Volume adjustment")}
        aria-disabled={disabled}
        aria-valuemin={0}
        aria-valuemax={maxPercent}
        aria-valuenow={amplitude === null || invalidDraft ? undefined : amplitude * 100}
        aria-valuetext={
          invalidDraft ? t("Invalid volume draft") : custom ? t("Volume is controlled by keyframes") : amplitude === null
            ? t("Mixed values")
            : `${displayVolume(amplitude * 100)}%`
        }
        onPointerDown={(event) => {
          if (disabled || event.button !== 0 || pointer.current !== null)
            return;
          event.preventDefault();
          event.currentTarget.focus();
          pointer.current = event.pointerId;
          event.currentTarget.setPointerCapture(event.pointerId);
          update(pointerValue(event.currentTarget, event.clientX));
        }}
        onPointerMove={(event) => {
          if (pointer.current === event.pointerId)
            update(pointerValue(event.currentTarget, event.clientX));
        }}
        onPointerUp={(event) => {
          if (pointer.current !== event.pointerId) return;
          const value = pointerValue(event.currentTarget, event.clientX);
          pointer.current = null;
          if (event.currentTarget.hasPointerCapture(event.pointerId))
            event.currentTarget.releasePointerCapture(event.pointerId);
          update(value);
          void save(value);
        }}
        onPointerCancel={() => reset()}
        onLostPointerCapture={() => {
          if (pointer.current !== null) reset();
        }}
        onBlur={() => {
          if (key.current !== null || pointer.current !== null) reset();
        }}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === "Escape") {
            event.preventDefault();
            reset();
            return;
          }
          if (
            disabled ||
            ![
              "ArrowLeft",
              "ArrowRight",
              "ArrowUp",
              "ArrowDown",
              "Home",
              "End",
            ].includes(event.key)
          )
            return;
          event.preventDefault();
          if (amplitude === null && !["Home", "End"].includes(event.key))
            return;
          key.current ??= event.key;
          const percent =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? maxPercent
                : Math.max(
                    0,
                    Math.min(
                      maxPercent,
                      amplitude! * 100 +
                        (["ArrowRight", "ArrowUp"].includes(event.key)
                          ? 1
                          : -1) *
                          (event.shiftKey ? 10 : 1),
                    ),
                  );
          update({ field: "percent", value: displayVolume(percent) });
        }}
        onKeyUp={(event) => {
          if (key.current === event.key) {
            event.preventDefault();
            event.stopPropagation();
            key.current = null;
            void save();
          }
        }}
      >
        <span className="workbench-volume-slider-track" />
        {amplitude !== null && !invalidDraft && (
          <span
            className="workbench-volume-slider-thumb"
            style={{
              left: `${Math.max(0, Math.min(100, ((amplitude * 100) / maxPercent) * 100))}%`,
            }}
          />
        )}
      </div>
      <p className="muted">
        {t(
          "Enter to save volume. Esc to restore. Release the slider to save once.",
        )}
      </p>
      {targets.some((target) => target.animated) && (
        <p className="muted">
          {t(
            "Adjusts the fade level while preserving its timing and gain unit.",
          )}
        </p>
      )}
      {custom && (
        <p className="muted">
          {t("Edit this custom volume envelope with the keyframe controls.")}
        </p>
      )}
      {targets.some(
        (target) =>
          target.track.muted || target.sourceDisabled || target.audio.muted,
      ) && (
        <p className="muted">
          {t(
            "Volume does not change track mute, clip mute or source audio settings.",
          )}
        </p>
      )}
      {blocked && <p className="muted">{t(blocked)}</p>}
      {loading && <p className="muted" role="status">{t("Checking volume editing availability…")}</p>}
      {!available && capabilities && (
        <p className="muted">
          {t("Volume editing is unavailable for this selection.")}
        </p>
      )}
      {pending && <p role="status">{t("Saving volume…")}</p>}
      {dirty && !pending && (
        <button type="button" onClick={reset}>
          {t("Cancel volume draft")}
        </button>
      )}
      {error instanceof VolumeInputError ? <p role="alert" id={`${owner}-error`}>{t(error.message)}</p> : error != null && (
        <ErrorNotice
          error={error}
          localSession={controller.canStartLocalSession}
        />
      )}
    </section>
  );
}
