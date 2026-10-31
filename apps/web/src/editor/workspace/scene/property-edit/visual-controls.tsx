import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { useEffect, useId, useRef, useState } from "react";
import { useEditingWorkspace } from "../../context";
import { t, useLocale } from "../../../shared/locale";
import { ErrorNotice, localSessionExpired } from "../../../shared/error-notice";
import { same } from "../audio-gain-plan";
import { editRecoveryBlocked } from "../edit-recovery";
import { usePropertyDraft } from "./use-property-draft";
import { planVisualProperty, type VisualPropertyPlan } from "./visual-property-plan";
import { OpacitySlider } from "./opacity-slider";
import {
  animatedVisualField, commonVisualValue, expectedVisualElement, fieldValue,
  parseVisualValue, selectionIdentity, visualFields, visualTargets,
} from "./visual-properties";
import type { VisualField } from "./visual-properties";

type Drafts = Partial<Record<VisualField, string>>;
export function VisualPropertyControls({ elementIds }: { elementIds: readonly string[] }) {
  const { api, scene } = useEditingWorkspace();
  const key = JSON.stringify([api?.connectionIdentity, scene?.head.project.id, scene?.sequenceId,
    scene?.historical?.revision ?? null, scene && selectionIdentity(scene), elementIds]);
  return <VisualForm key={key} elementIds={elementIds} />;
}
function VisualForm({ elementIds }: { elementIds: readonly string[] }) {
  useLocale();
  const { api, scene, controller, busy } = useEditingWorkspace();
  const observedIdentity = controller.scene.timelineEdit.identity();
  const capture = () => {
    const state = controller.getSnapshot(), view = state.scene;
    return { api: state.api, identity: controller.scene.timelineEdit.identity(),
      project: view?.head.project.id, revision: (view?.historical ?? view?.head)?.revision,
      sequence: view?.sequenceId, selection: view ? selectionIdentity(view) : "",
      targets: view ? structuredClone(visualTargets(view, elementIds)) : null };
  };
  const [baseline, setBaseline] = useState(capture), base = useRef(baseline);
  const [drafts, setDrafts] = useState<Drafts>({}), values = useRef<Drafts>({});
  const [sliderActive, setSliderActive] = useState(false);
  const [tools, setTools] = useState<readonly string[] | null>(null),
    [error, setError] = useState<unknown>(null),
    [pending, setPending] = useState(false),
    [handedOff, setHandedOff] = useState(false),
    [invalid, setInvalid] = useState<VisualField | null>(null);
  const alive = useRef(false), flight = useRef(false), abort = useRef<AbortController | null>(null),
    root = useRef<HTMLElement>(null), composing = useRef(false), focusAfterSave = useRef<VisualField | null>(null);
  const dirty = Object.keys(drafts).length > 0;
  const recoveryPending = editRecoveryBlocked(scene?.recovery) || controller.scene.recovery.blocked();
  // Durable recovery temporarily owns the submitted intent. When it finishes,
  // unsent fields must fence selection/writes again, while remaining frozen.
  const owner = usePropertyDraft(controller, (dirty || sliderActive) && !(handedOff && recoveryPending));
  const errorId = useId();
  const assign = (next: Drafts) => { values.current = next; setDrafts(next); };
  const adopt = (next: typeof baseline) => { base.current = next; setBaseline(next); };
  useEffect(() => {
    alive.current = true;
    if (api) editorHostCapabilities(api).then(c => { if (alive.current) setTools(c.tools); })
      .catch(failure => { if (alive.current) { setError(failure); if (localSessionExpired(failure)) controller.report(failure); } });
    return () => { alive.current = false; abort.current?.abort(); };
  }, [api, controller]);
  // A pristine view follows authoritative refreshes. Dirty values never rebase
  // across a foreign revision; only the exact successful own receipt below can.
  useEffect(() => {
    if (flight.current || Object.keys(values.current).length) return;
    if (handedOff) {
      if (editRecoveryBlocked(scene?.recovery) || controller.scene.recovery.blocked()) return;
      setHandedOff(false);
    }
    if (base.current.revision !== (scene?.historical ?? scene?.head)?.revision ||
        base.current.identity.generation !== observedIdentity.generation ||
        base.current.identity.viewGeneration !== observedIdentity.viewGeneration) adopt(capture());
  }, [scene?.head.revision, scene?.historical?.revision, scene?.recovery, dirty, handedOff,
    observedIdentity.generation, observedIdentity.viewGeneration]);
  useEffect(() => {
    if (pending || !focusAfterSave.current) return;
    root.current?.querySelector<HTMLInputElement>(`[data-visual-field="${focusAfterSave.current}"]`)?.focus({ preventScroll: true });
    focusAfterSave.current = null;
  }, [pending, baseline.revision]);
  const reason = () => {
    const captured = base.current, state = controller.getSnapshot(), view = state.scene,
      identity = controller.scene.timelineEdit.identity();
    if (view?.historical) return "Return to the current revision before editing.";
    if (!alive.current || !view || state.api !== captured.api ||
        identity.generation !== captured.identity.generation || identity.viewGeneration !== captured.identity.viewGeneration ||
        view.head.project.id !== captured.project || view.head.revision !== captured.revision ||
        view.sequenceId !== captured.sequence || selectionIdentity(view) !== captured.selection || view.externalRevision)
      return "The property target changed. Confirm no more fields; restore your drafts before refreshing.";
    const targets = visualTargets(view, elementIds), sequence = view.head.project.sequences.find(s => s.id === view.sequenceId);
    if (!targets || !same(targets, captured.targets) || targets.some(target => sequence?.tracks.find(track => track.id === target.trackId)?.locked))
      return "The property target changed or its track is locked.";
    if (handedOff || state.busy || view.gainDraft || view.propertyDrafts?.some(id => id !== owner) ||
        editRecoveryBlocked(view.recovery) || controller.scene.recovery.blocked() ||
        view.audioGain && view.audioGain.phase !== "saved" || view.sourceAudio && view.sourceAudio.phase !== "saved" ||
        view.timelineEdit.attempt || view.clipboard.attempt || view.media.attempt ||
        ["loading", "playing", "stalled", "unconfirmed"].includes(view.audioPreview.phase))
      return "Pause playback and confirm or cancel other edits before changing properties.";
    return null;
  };
  const blocked = tools === null ? null : reason(), available = tools?.includes("opencut_element_properties_plan");
  const disabled = !available || pending || busy || !!blocked;
  const restore = (field: VisualField) => {
    if (flight.current) return;
    const next = { ...values.current }; delete next[field]; assign(next); setInvalid(null); setError(null);
    controller.scene.propertyDraft(owner, Object.keys(next).length > 0 && !(handedOff && recoveryPending));
  };
  const prepare = (field: VisualField, number: number, signal: AbortSignal) => {
    const captured = base.current;
    return planVisualProperty({api:captured.api!, project:captured.project!, revision:captured.revision!,
      sequence:captured.sequence!, targets:captured.targets!, field, value:number, signal,
      check:() => { const changed=reason(); if (changed) throw Error(changed); }});
  };
  async function save(field: VisualField, prepared?: VisualPropertyPlan): Promise<boolean> {
    if (flight.current || values.current[field] === undefined) return false;
    const denied = reason();
    if (denied || !available) { setError(Error(denied ?? "Property editing is unavailable.")); return false; }
    const captured = base.current, targets = captured.targets!;
    if (animatedVisualField(targets, field)) return false;
    let number: number;
    try { number = parseVisualValue(field, values.current[field]!); }
    catch (failure) { setError(failure); setInvalid(field); return false; }
    if (targets.every(target => fieldValue(target.element, field) === number)) { restore(field); return true; }
    const signal = new AbortController(); abort.current = signal;
    flight.current = true; setPending(true); setError(null); setInvalid(null);
    let transaction: VisualPropertyPlan["transaction"] | null = prepared?.transaction ?? null;
    const expected = targets.map(target => ({ ...target, element: expectedVisualElement(target.element, field, number) }));
    try {
      const plan = prepared ?? await prepare(field, number, signal.signal);
      transaction = plan.transaction;
      if (plan.field !== field || plan.value !== number || !same(plan.expected, expected) ||
          transaction.project_id !== captured.project || transaction.expected_revision !== captured.revision)
        throw Error("The picture property plan changed fields outside the requested edit.");
      const changed = reason(); if (changed) throw Error(changed);
      const receipt = await controller.scene.applyPlanned(transaction, captured.sequence!, owner);
      if (!alive.current) return false;
      const state = controller.getSnapshot(), view = state.scene, actual = view && visualTargets(view, elementIds);
      if (!receipt || receipt.base_revision !== captured.revision || !receipt.transaction_id ||
          state.api !== captured.api || !view || view.head.project.id !== captured.project ||
          view.sequenceId !== captured.sequence || selectionIdentity(view) !== captured.selection || view.historical ||
          view.externalRevision || view.head.revision !== receipt.revision || !same(actual, expected))
        throw Error("The property target changed. Confirm no more fields; restore your drafts before refreshing.");
      adopt(capture());
      const remaining = { ...values.current }; delete remaining[field]; assign(remaining);
      // applyPlanned released this owner before refreshing its accepted head.
      // Preserve every other local field, then restore the shared write fence.
      controller.scene.propertyDraft(owner, Object.keys(remaining).length > 0);
      focusAfterSave.current = field;
      return true;
    } catch (failure) {
      if (!alive.current || signal.signal.aborted) return false;
      const record = controller.getSnapshot().scene?.recovery?.record;
      if (record?.context.intent.kind === "public_apply" && same(record.context.intent.request, transaction)) {
        setHandedOff(true); controller.scene.propertyDraft(owner, false);
      } else if (Object.keys(values.current).length) controller.scene.propertyDraft(owner, true);
      setError(failure); if (localSessionExpired(failure)) controller.report(failure);
      return false;
    } finally {
      flight.current = false;
      if (alive.current) setPending(false);
    }
  }
  if (!baseline.targets) return null;
  return <section ref={root} className="workbench-visual-controls" aria-label={t("Picture properties")} aria-busy={pending || tools === null && !error}>
    <h4>{t("Picture")}</h4>
    <div className="workbench-visual-inputs">
      {visualFields.map(field => {
        const animated = animatedVisualField(baseline.targets!, field.key), isDirty = drafts[field.key] !== undefined;
        return <div key={field.key}>
          <label><span>{t(field.label)}</span><span>
            <input type="number" step="any" data-visual-field={field.key} aria-label={t(field.label)} placeholder="—"
              aria-invalid={invalid === field.key} aria-describedby={invalid === field.key ? errorId : animated ? `${errorId}-${field.key}` : undefined}
              min={field.key === "opacity" ? 0 : undefined} max={field.key === "opacity" ? 100 : undefined}
              disabled={!available || pending || busy || sliderActive || animated || !!blocked && !isDirty} readOnly={!!blocked && isDirty}
              title={animated ? t("Edit this animated property with the keyframe controls.") : undefined}
              value={drafts[field.key] ?? commonVisualValue(baseline.targets!, field.key)}
              onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }}
              onChange={event => {
                const next = { ...values.current }, value = event.target.value;
                if (value === commonVisualValue(base.current.targets!, field.key)) delete next[field.key]; else next[field.key] = value;
                assign(next); setError(null); setInvalid(null);
              }}
              onKeyDown={event => {
                event.stopPropagation();
                if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
                if (event.key === "Enter") { event.preventDefault(); void save(field.key); }
                if (event.key === "Escape") { event.preventDefault(); restore(field.key); }
              }} /><span aria-hidden="true">{field.unit}</span>
          </span></label>
          {isDirty && <div className="workbench-visual-field-actions">
            <button type="button" disabled={disabled || sliderActive} aria-label={t("Confirm {field}", { field: t(field.label) })} onClick={() => void save(field.key)}>{t("Confirm")}</button>
            <button type="button" disabled={pending || sliderActive} aria-label={t("Restore {field}", { field: t(field.label) })} onClick={() => restore(field.key)}>{t("Restore")}</button>
          </div>}
          {animated && <p className="muted" id={`${errorId}-${field.key}`}>{t("Edit this animated property with the keyframe controls.")}</p>}
        </div>;
      })}
    </div>
    {tools?.includes("opencut_visual_preview_edit_draft_frame") && <OpacitySlider controller={controller} targets={baseline.targets}
      value={drafts.opacity ?? commonVisualValue(baseline.targets, "opacity")}
      disabledReason={blocked ?? (!available || !tools?.includes("opencut_visual_preview_edit_draft_frame") ? "Opacity preview is unavailable." :
        animatedVisualField(baseline.targets, "opacity") ? "Edit this animated property with the keyframe controls." :
        !sliderActive && drafts.opacity !== undefined ? "Confirm or restore the typed opacity before dragging." : null)}
      prepare={(value, signal) => prepare("opacity", value, signal)} commit={plan => save("opacity", plan)}
      own={active => { if (active) controller.scene.propertyDraft(owner, true); setSliderActive(active); }}
      change={value => { assign({...values.current, opacity:value}); setError(null); setInvalid(null); }}
      restore={() => restore("opacity")} report={failure => { setError(failure); if (localSessionExpired(failure)) controller.report(failure); }} />}
    <p className="muted">{t("Enter confirms this field; Esc restores it. Other field drafts remain.")}</p>
    {tools === null && !error && <p role="status">{t("Checking picture editing availability…")}</p>}
    {tools && !available && <p className="muted">{t("Property editing is unavailable.")}</p>}
    {blocked && (dirty || blocked !== "The property target changed. Confirm no more fields; restore your drafts before refreshing.") && <p className="muted">{t(blocked)}</p>}
    {invalid && error ? <p role="alert" id={errorId}>{t(error instanceof Error ? error.message : String(error))}</p> : error ? <ErrorNotice error={error} localSession={controller.canStartLocalSession} /> : null}
  </section>;
}
