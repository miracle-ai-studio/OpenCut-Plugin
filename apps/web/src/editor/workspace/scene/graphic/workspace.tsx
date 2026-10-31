import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { Select } from "../../../shared/controls";
import { createContext, useContext, useEffect, useRef, useState } from "react";
import { useWorkbenchView } from "../workbench/view";
import {graphicDraft,graphicDraftValues} from "./draft";
import type { ReactNode } from "react";
import { useEditingWorkspace as useWorkspace } from "../../context";
import { Button, Field } from "../../../shared/ui";
import { t, useLocale } from "../../../shared/locale";
import { secondsForDisplay } from "../../../model/time";
import { editRecoveryBlocked } from "../edit-recovery";
import { GraphicEditor } from "./editor";
import { checkGraphicPlan } from "./planning";
import { graphicKinds } from "./types";
import { Circle, Hexagon, RectangleHorizontal, Star,Plus } from "lucide-react";
import type {
  GraphicInitial,
  GraphicKind,
  GraphicPlanRequest,
  GraphicPlanResult,
  GraphicScope,
  GraphicValues,
} from "./types";
type Draft = {
  mode: "create" | "edit";
  scope: GraphicScope;
  initial: GraphicInitial;
  elementId: string;
  trackId: string | null;
};
type Context = {
  available: boolean;
  disabled: boolean;
  beginCreate(kind: GraphicKind, insert?:boolean): void;
  beginEdit(id: string): void;
  panel: ReactNode;
};
const GraphicContext = createContext<Context | null>(null);
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
const shapeLabel = (kind: string) =>
  t(
    kind === "rectangle"
      ? "Rectangle"
      : kind === "ellipse"
        ? "Ellipse"
        : kind === "polygon"
          ? "Polygon"
          : "Star",
  );
export function GraphicAuthoringProvider({
  children,
}: {
  children: ReactNode;
}) {
  useLocale();
  const { api, scene, busy, controller } = useWorkspace();
  const [available, setAvailable] = useState(false),
    [draft, setDraft] = useState<Draft | null>(null),
    [review, setReview] = useState<GraphicPlanResult | null>(null),
    [pending, setPending] = useState(false),
    [error, setError] = useState<string | null>(null);
  const epoch = useRef(0),
    reading = useRef<AbortController | null>(null),
    writing = useRef(false);
  const sequence = (scene?.historical ?? scene?.head)?.project.sequences.find(
    (s) => s.id === scene?.sequenceId,
  );
  const disabled =
    busy ||
    pending ||
    !!scene?.historical ||
    editRecoveryBlocked(scene?.recovery) ||
    !!scene?.recovery?.record ||
    !!scene?.recovery?.error ||
    ["reading", "staging", "applying"].includes(scene?.recovery?.phase ?? "") ||
    controller.scene.recovery.blocked();
  useEffect(() => {
    let live = true;
    setAvailable(false);
    if (api)
      void editorHostCapabilities(api)
        .then((value) => {
          if (live)
            setAvailable(value.tools.includes("opencut_graphic_plan"));
        })
        .catch((value) => {
          if (live) setError(message(value));
        });
    return () => {
      live = false;
    };
  }, [api, api?.connectionIdentity]);
  useEffect(() => {
    epoch.current++;
    reading.current?.abort();
    setDraft(null);
    setReview(null);
    setPending(false);
    setError(null);
    return () => {
      epoch.current++;
      reading.current?.abort();
    };
  }, [
    api,
    api?.connectionIdentity,
    scene?.head.project.id,
    scene?.head.revision,
    scene?.historical?.revision,
    scene?.sequenceId,
    scene?.selectedId,
  ]);
  const currentScope = (): GraphicScope => {
    const state = controller.getSnapshot();
    if (
      !api ||
      state.api !== api ||
      !state.scene ||
      state.scene.historical ||
      !available
    )
      throw new Error("The edit target changed.");
    return {
      projectId: state.scene.head.project.id,
      revision: state.scene.head.revision,
      sequenceId: state.scene.sequenceId,
    };
  };
  const checkScope = (scope: GraphicScope, serial: number) => {
    const current = currentScope();
    if (
      serial !== epoch.current ||
      current.projectId !== scope.projectId ||
      current.revision !== scope.revision ||
      current.sequenceId !== scope.sequenceId
    )
      throw new Error("The edit target changed.");
  };
  const beginCreate = (geometry: GraphicKind,insert=false) => {
    if (disabled || !available || !api || !scene) return;
    reading.current?.abort();
    const abort = new AbortController(),
      serial = ++epoch.current,
      scope = currentScope();
    reading.current = abort;
    setPending(true);
    setError(null);
    const request: GraphicPlanRequest = {
      project_id: scope.projectId,
      expected_revision: scope.revision,
      sequence_id: scope.sequenceId,
      idempotency_key: crypto.randomUUID(),
      command: {
        kind: "defaults",
        geometry,
        start: { kind: "exact", time: structuredClone(controller.scene.currentPlayhead()) },
      },
    };
    void (async () => {
      try {
        const result = await api.sdk.call("opencut_graphic_plan", request, {
          signal: abort.signal,
        });
        checkScope(scope, serial);
        checkGraphicPlan(request, result);
        if(insert){
          const create:GraphicPlanRequest={...request,idempotency_key:crypto.randomUUID(),command:{kind:"create",element_id:crypto.randomUUID(),track:{kind:"new",track_id:crypto.randomUUID()},values:graphicDraftValues(result.initial!,graphicDraft(result.initial!))}};
          const planned=await api.sdk.call("opencut_graphic_plan",create,{signal:abort.signal});
          checkScope(scope,serial);checkGraphicPlan(create,planned);
          if(!planned.transaction)throw Error("No graphic changes to save.");
          writing.current=true;
          try{await controller.scene.applyPlanned(planned.transaction,scope.sequenceId);}finally{writing.current=false;}
          const current=controller.getSnapshot();
          if(current.api===api&&current.scene?.head.project.id===scope.projectId&&current.scene.sequenceId===scope.sequenceId&&!current.scene.historical&&planned.selection)controller.scene.select(planned.selection.element_id);
          return;
        }
        setReview(null);
        setDraft({
          mode: "create",
          scope,
          initial: result.initial!,
          elementId: crypto.randomUUID(),
          trackId: null,
        });
      } catch (value) {
        if (serial === epoch.current && !abort.signal.aborted)
          setError(message(value));
      } finally {
        if (serial === epoch.current) setPending(false);
      }
    })();
  };
  const beginEdit = (id: string) => {
    if (disabled || !available || !sequence) return;
    const track = sequence.tracks.find((t) =>
        t.elements.some((e) => e.id === id),
      ),
      element = track?.elements.find((e) => e.id === id);
    if (
      !track ||
      track.locked ||
      !element ||
      element.content.kind !== "graphic" ||
      !element.visual ||
      !["rectangle", "ellipse", "polygon", "star"].includes(
        element.content.graphic.geometry.kind,
      )
    )
      return;
    reading.current?.abort();
    epoch.current++;
    setError(null);
    setReview(null);
    setDraft({
      mode: "edit",
      scope: currentScope(),
      elementId: id,
      trackId: track.id,
      initial: structuredClone({
        name: element.name,
        graphic: element.content.graphic,
        transform: element.visual.transform,
        opacity: element.visual.opacity,
        timeline_range: element.timeline_range,
      }),
    });
  };
  const plan = async (values: GraphicValues) => {
    if (!draft || !api || disabled || writing.current)
      throw new Error("Wait for the current edit to finish.");
    const serial = epoch.current;
    checkScope(draft.scope, serial);
    const abort = new AbortController();
    reading.current = abort;
    setPending(true);
    setError(null);
    const request: GraphicPlanRequest = {
      project_id: draft.scope.projectId,
      expected_revision: draft.scope.revision,
      sequence_id: draft.scope.sequenceId,
      idempotency_key: crypto.randomUUID(),
      command:
        draft.mode === "edit"
          ? {
              kind: "edit",
              target: { track_id: draft.trackId!, element_id: draft.elementId },
              values,
            }
          : {
              kind: "create",
              element_id: draft.elementId,
              track: draft.trackId
                ? { kind: "existing", track_id: draft.trackId }
                : { kind: "new", track_id: crypto.randomUUID() },
              values,
            },
    };
    try {
      const result = await api.sdk.call("opencut_graphic_plan", request, {
        signal: abort.signal,
      });
      checkScope(draft.scope, serial);
      checkGraphicPlan(request, result);
      setDraft({ ...draft, initial: result.initial! });
      setReview(result);
      if (!result.transaction) setError("No graphic changes to save.");
    } finally {
      if (serial === epoch.current) setPending(false);
    }
  };
  const apply = async () => {
    if (!draft || !review?.transaction || disabled || writing.current) return;
    const captured = review,
      scope = draft.scope,
      serial = epoch.current;
    writing.current = true;
    setPending(true);
    setError(null);
    try {
      checkScope(scope, serial);
      await controller.scene.applyPlanned(captured.transaction!, scope.sequenceId);
      const state = controller.getSnapshot();
      if (
        state.api === api &&
        state.scene?.head.project.id === scope.projectId &&
        state.scene.sequenceId === scope.sequenceId &&
        !state.scene.historical &&
        captured.selection
      )
        controller.scene.select(captured.selection.element_id);
      if (serial === epoch.current) {
        setDraft(null);
        setReview(null);
      }
    } catch (value) {
      if (serial === epoch.current) setError(message(value));
      else controller.report(value);
    } finally {
      writing.current = false;
      if (serial === epoch.current) setPending(false);
    }
  };
  const panel =
    available && draft ? (
      <section className="editor-text-authoring" aria-label={t("Graphic editing")}>
        {error && <p role="alert">{t(error)}</p>}
        {draft.mode === "create" && (
          <Field label={t("Graphic track")}>
            <Select
              disabled={disabled}
              value={draft.trackId ?? ""}
              onChange={(event) => {
                setDraft({ ...draft, trackId: event.target.value || null });
                setReview(null);
              }}
            >
              <option value="">{t("New graphic track")}</option>
              {sequence?.tracks
                .filter((track) => track.kind === "graphic" && !track.locked)
                .map((track) => (
                  <option key={track.id} value={track.id}>
                    {track.name}
                  </option>
                ))}
            </Select>
          </Field>
        )}
        {!review?.transaction ? (
          <GraphicEditor
            key={draft.elementId}
            initial={draft.initial}
            disabled={disabled}
            save={plan}
            close={() => {
              if (!writing.current) {
                setDraft(null);
                setReview(null);
              }
            }}
          />
        ) : (
          <section aria-label={t("Review graphic changes")}>
            <h3>{t("Review graphic changes")}</h3>
            <p>
              {review.initial!.name} ·{" "}
              {shapeLabel(review.initial!.graphic.geometry.kind)} ·{" "}
              {secondsForDisplay(review.initial!.timeline_range.start)} s ·{" "}
              {secondsForDisplay(review.initial!.timeline_range.duration)} s
            </p>
            <p>{t("The project changes only after you apply this plan.")}</p>
            <div className="toolbar">
              <Button disabled={disabled} onClick={() => void apply()}>
                {t("Apply graphic changes")}
              </Button>
              <Button disabled={disabled} onClick={() => setReview(null)}>
                {t("Back to graphic edit")}
              </Button>
              <Button
                disabled={disabled}
                onClick={() => {
                  setReview(null);
                  setDraft(null);
                }}
              >
                {t("Cancel")}
              </Button>
            </div>
          </section>
        )}
      </section>
    ) : error ? (
      <p role="alert">{t(error)}</p>
    ) : null;
  return (
    <GraphicContext.Provider
      value={{ available, disabled, beginCreate, beginEdit, panel }}
    >
      {children}
    </GraphicContext.Provider>
  );
}
export function GraphicActions({
  allowedKinds = graphicKinds,
  cards=false,
}: { allowedKinds?: readonly GraphicKind[];cards?:boolean } = {}) {
  useLocale();
  const { showProperties } = useWorkbenchView();
  const context = useContext(GraphicContext),
    [kind, setKind] = useState<GraphicKind>("rectangle");
  if (!context?.available) return null;
  const kinds = graphicKinds.filter(value => allowedKinds.includes(value)),
    selected = kinds.includes(kind) ? kind : kinds[0];
  if (!selected) return <p role="status">{t("No matching shapes.")}</p>;
  return (
    <>
      <div className="workbench-shape-grid" role="group" aria-label={t("Graphic shape")}>
        {kinds.map((value) => {
          const Icon = { rectangle: RectangleHorizontal, ellipse: Circle, polygon: Hexagon, star: Star }[value];
          return <div className={cards?"resource-shape-card":"resource-shape-option"} key={value}><button type="button" disabled={context.disabled}
            aria-pressed={selected === value} onClick={() => setKind(value)}>
            <Icon size={30} strokeWidth={1.5} aria-hidden="true" />
            <span>{shapeLabel(value)}</span>
          </button>{cards&&<Button className="resource-tile-add" aria-label={`${t("Add graphic")} · ${shapeLabel(value)}`} title={t("Add graphic")} disabled={context.disabled}
            onClick={()=>{showProperties();context.beginCreate(value,true);}} icon={<Plus size={14}/>}/>}</div>;
        })}
      </div>
      {!cards&&<Button
        className="primary workbench-resource-add"
        disabled={context.disabled}
        onClick={() => { showProperties(); context.beginCreate(selected); }}
      >
        {t("Add graphic")}
      </Button>}
    </>
  );
}
export function GraphicEditButton({
  elementId,
  locked,
}: {
  elementId: string;
  locked: boolean;
}) {
  useLocale();
  const context = useContext(GraphicContext);
  return context?.available ? (
    <Button
      disabled={locked || context.disabled}
      onClick={() => context.beginEdit(elementId)}
    >
      {t("Edit graphic")}
    </Button>
  ) : null;
}
export function GraphicPanel() {
  return useContext(GraphicContext)?.panel ?? null;
}
