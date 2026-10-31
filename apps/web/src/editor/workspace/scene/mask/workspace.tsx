import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { createContext, useContext, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useEditingWorkspace as useWorkspace } from "../../context";
import { t, useLocale } from "../../../shared/locale";
import { editRecoveryBlocked } from "../edit-recovery";
import { MaskEditor } from "./editor";
import { isBoxMask, maskTargetProblem } from "./types";
import type { BoxMask, MaskGeometryPlan, MaskKind } from "./types";
import type { Element } from "../types";
import type { EditorApi } from "../../api";
import "./mask.css";

type Draft = {
  key: number;
  api: EditorApi;
  connection: string | null;
  projectId: string;
  revision: string;
  sequenceId: string;
  selectedId: string | null;
  trackId: string;
  element: Element;
  timeKey: string;
  plan: MaskGeometryPlan;
  mask: BoxMask;
};
type Context = {
  available: boolean;
  disabled: boolean;
  pending: boolean;
  error: string | null;
  draft: Draft | null;
  beginEdit(id: string, kind?: MaskKind): void;
  change(mask: BoxMask): void;
  save(mask?: BoxMask): Promise<void>;
  cancel(): void;
  valid(): boolean;
};
const MaskContext = createContext<Context | null>(null);
export const useMaskAuthoring = () => useContext(MaskContext);
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export function MaskAuthoringProvider({ children }: { children: ReactNode }) {
  const { api, scene, controller, busy } = useWorkspace();
  const [available, setAvailable] = useState(false),
    [pending, setPending] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null),
    [error, setError] = useState<string | null>(null);
  const current = useRef<Draft | null>(null),
    epoch = useRef(0),
    writing = useRef(false),
    reading = useRef<AbortController | null>(null);
  const publish = (next: Draft | null) => {
    current.current = next;
    setDraft(next);
  };
  const recoveryBlocked = () => {
    const state = controller.getSnapshot(),
      view = state.scene;
    return (
      !view ||
      state.busy ||
      !!view.gainDraft ||
      !!view.historical ||
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        view.audioPreview.phase,
      ) ||
      editRecoveryBlocked(view.recovery) ||
      controller.scene.recovery.blocked() ||
      !!view.recovery?.record ||
      !!view.recovery?.error ||
      ["reading", "staging", "applying"].includes(view.recovery?.phase ?? "") ||
      ["applying", "unconfirmed"].includes(
        view.timelineEdit.attempt?.phase ?? "",
      ) ||
      ["applying", "unconfirmed"].includes(view.clipboard.attempt?.phase ?? "")
    );
  };
  const disabled = busy || pending || recoveryBlocked();
  useEffect(() => {
    let live = true;
    setAvailable(false);
    if (api)
      void editorHostCapabilities(api)
        .then((result) => {
          if (live)
            setAvailable(result.tools.includes("opencut_mask_geometry"));
        })
        .catch((cause) => {
          if (live) setError(errorText(cause));
        });
    return () => {
      live = false;
    };
  }, [api, api?.connectionIdentity]);
  const timeKey = JSON.stringify(scene?.playhead);
  useEffect(() => {
    epoch.current++;
    reading.current?.abort();
    publish(null);
    setPending(false);
    setError(null);
    return () => {
      epoch.current++;
      reading.current?.abort();
      current.current = null;
    };
  }, [
    api,
    api?.connectionIdentity,
    scene?.head.project.id,
    scene?.head.revision,
    scene?.historical?.revision,
    scene?.sequenceId,
    scene?.selectedId,
    timeKey,
  ]);

  const reason = (captured: Omit<Draft, "plan" | "mask">): string | null => {
    const state = controller.getSnapshot(),
      view = state.scene;
    if (
      !view ||
      state.api !== captured.api ||
      state.api.connectionIdentity !== captured.connection ||
      view.head.project.id !== captured.projectId ||
      view.head.revision !== captured.revision ||
      view.sequenceId !== captured.sequenceId ||
      view.selectedId !== captured.selectedId ||
      JSON.stringify(view.playhead) !== captured.timeKey ||
      captured.key !== epoch.current
    )
      return "The mask target changed. Reopen its controls before editing.";
    if (recoveryBlocked())
      return "Finish playback or resolve the current edit before editing a mask.";
    const track = view.head.project.sequences
      .find((scene) => scene.id === captured.sequenceId)
      ?.tracks.find((track) => track.id === captured.trackId);
    const element = track?.elements.find(
      (element) => element.id === captured.element.id,
    );
    if (!track || track.locked || !track.enabled || !element?.enabled)
      return "Unlock and enable the target track and element before editing a mask.";
    if (JSON.stringify(element) !== JSON.stringify(captured.element))
      return "The mask target changed. Reopen its controls before editing.";
    return maskTargetProblem(element, track.channels.length);
  };
  const cancel = () => {
    if (writing.current) return;
    epoch.current++;
    reading.current?.abort();
    publish(null);
    setPending(false);
    setError(null);
  };
  const valid = () => {
    const value = current.current;
    return !!value && !writing.current && !reason(value);
  };
  const beginEdit = (id: string, kind?: MaskKind) => {
    if (!available || disabled || writing.current) return;
    const state = controller.getSnapshot(),
      view = state.scene;
    if (!state.api || !view || recoveryBlocked()) return;
    const track = view.head.project.sequences
      .find((scene) => scene.id === view.sequenceId)
      ?.tracks.find((track) =>
        track.elements.some((element) => element.id === id),
      );
    const element = track?.elements.find((element) => element.id === id);
    if (!element || !track) return;
    const key = ++epoch.current,
      captured = {
        key,
        api: state.api,
        connection: state.api.connectionIdentity,
        projectId: view.head.project.id,
        revision: view.head.revision,
        sequenceId: view.sequenceId,
        selectedId: view.selectedId,
        trackId: track.id,
        element: structuredClone(element),
        timeKey: JSON.stringify(view.playhead),
      };
    const problem = reason(captured);
    if (problem) {
      setError(problem);
      return;
    }
    const existing = element.visual!.masks.find((mask) => mask.enabled);
    if ((existing && kind) || (!existing && !kind)) {
      setError("The mask target changed. Reopen its controls before editing.");
      return;
    }
    reading.current?.abort();
    const abort = new AbortController();
    reading.current = abort;
    publish(null);
    setPending(true);
    setError(null);
    void (async () => {
      try {
        const plan = await captured.api.sdk.call(
          "opencut_mask_geometry",
          {
            project_id: captured.projectId,
            expected_revision: captured.revision,
            sequence_id: captured.sequenceId,
            element_id: element.id,
            time: structuredClone(view.playhead),
          },
          { signal: abort.signal },
        );
        if (abort.signal.aborted || key !== epoch.current) return;
        const changed = reason(captured);
        if (changed) throw new Error(changed);
        if (
          plan.revision !== captured.revision ||
          plan.element_id !== element.id ||
          ![
            plan.quad.centerX,
            plan.quad.centerY,
            plan.quad.width,
            plan.quad.height,
            plan.quad.rotationRadians,
            plan.quad.opacity,
          ].every(Number.isFinite) ||
          plan.quad.width <= 0 ||
          plan.quad.height <= 0 ||
          typeof plan.quad.flipX !== "boolean" ||
          typeof plan.quad.flipY !== "boolean" ||
          !Object.values(plan.default_geometry).every(Number.isFinite) ||
          plan.default_geometry.width <= 0 ||
          plan.default_geometry.height <= 0 ||
          plan.default_geometry.scale < 0
        )
          throw new Error(
            "Mask geometry differs from the requested fixed revision.",
          );
        const mask = existing ?? {
          id: crypto.randomUUID(),
          enabled: true,
          inverted: false,
          shape: {
            kind: kind!,
            geometry: structuredClone(plan.default_geometry),
          },
          feather: { unit: "classic_pixels" as const, value: 0 },
          stroke: {
            color: {
              encoding: "srgb" as const,
              red: 1,
              green: 1,
              blue: 1,
              alpha: 1,
            },
            width_px: 0,
            align: "center" as const,
          },
        };
        if (!isBoxMask(mask))
          throw new Error(
            "This mask cannot be edited by the box-mask controls.",
          );
        publish({ ...captured, plan, mask: structuredClone(mask) });
      } catch (cause) {
        if (!abort.signal.aborted && key === epoch.current)
          setError(errorText(cause));
      } finally {
        if (key === epoch.current) setPending(false);
      }
    })();
  };
  const change = (mask: BoxMask) => {
    const value = current.current;
    if (!value || !valid()) return;
    if (mask.id !== value.mask.id || !isBoxMask(mask)) return;
    publish({ ...value, mask: structuredClone(mask) });
    setError(null);
  };
  const save = async (replacement?: BoxMask) => {
    const value = current.current;
    if (!value || writing.current) return;
    const problem = reason(value);
    if (problem) {
      setError(problem);
      return;
    }
    const mask = replacement ?? value.mask,
      g = mask.shape.geometry;
    if (
      !isBoxMask(mask) ||
      mask.id !== value.mask.id ||
      !Object.values(g).every(Number.isFinite) ||
      g.width <= 0 ||
      g.height <= 0 ||
      g.scale < 0 ||
      !Number.isFinite(mask.feather.value) ||
      mask.feather.value < 0 ||
      mask.feather.value > 1000
    ) {
      setError(
        "Enter valid mask dimensions and a feather between 0 and 1000 pixels.",
      );
      return;
    }
    const element = structuredClone(value.element),
      masks = element.visual!.masks;
    const index = masks.findIndex((item) => item.id === mask.id);
    if (index < 0) masks.push(structuredClone(mask));
    else masks[index] = structuredClone(mask);
    if (JSON.stringify(element) === JSON.stringify(value.element)) {
      cancel();
      return;
    }
    writing.current = true;
    setPending(true);
    setError(null);
    try {
      await controller.scene.write("Edit mask", [
        {
          type: "element_put",
          sequence_id: value.sequenceId,
          track_id: value.trackId,
          element,
        },
      ]);
      if (value.key === epoch.current) {
        epoch.current++;
        publish(null);
      }
    } catch (cause) {
      if (value.key === epoch.current) setError(errorText(cause));
    } finally {
      writing.current = false;
      if (!current.current || value.key === epoch.current) setPending(false);
    }
  };
  return (
    <MaskContext.Provider
      value={{
        available,
        disabled,
        pending,
        error,
        draft,
        beginEdit,
        change,
        save,
        cancel,
        valid,
      }}
    >
      {children}
    </MaskContext.Provider>
  );
}

export function MaskInspector({
  elementId,
  locked = false,
}: {
  elementId: string;
  locked?: boolean;
}) {
  useLocale();
  const authoring = useMaskAuthoring(),
    { scene } = useWorkspace();
  const sequence = (scene?.historical ?? scene?.head)?.project.sequences.find(
    (sequence) => sequence.id === scene?.sequenceId,
  );
  const track = sequence?.tracks.find((track) =>
    track.elements.some((element) => element.id === elementId),
  );
  const element = track?.elements.find((element) => element.id === elementId);
  if (!authoring || !element) return null;
  const problem = maskTargetProblem(element, track?.channels.length);
  const enabled = element.visual?.masks.some((mask) => mask.enabled);
  return (
    <section aria-label={t("Mask controls")}>
      <h3>{t("Mask")}</h3>
      {!authoring.available && (
        <p role="status">{t("Mask editing is unavailable on this host.")}</p>
      )}
      {problem && <p role="status">{t(problem)}</p>}
      <div className="editor-mask-actions">
        {enabled ? (
          <button
            type="button"
            disabled={
              !authoring.available || authoring.disabled || locked || !!problem
            }
            onClick={() => authoring.beginEdit(elementId)}
          >
            {t("Edit mask")}
          </button>
        ) : (
          <>
            <button
              type="button"
              disabled={
                !authoring.available ||
                authoring.disabled ||
                locked ||
                !!problem
              }
              onClick={() => authoring.beginEdit(elementId, "rectangle")}
            >
              {t("Add rectangle mask")}
            </button>
            <button
              type="button"
              disabled={
                !authoring.available ||
                authoring.disabled ||
                locked ||
                !!problem
              }
              onClick={() => authoring.beginEdit(elementId, "ellipse")}
            >
              {t("Add ellipse mask")}
            </button>
          </>
        )}
      </div>
      {authoring.error && !authoring.draft && (
        <p role="alert">{t(authoring.error)}</p>
      )}
    </section>
  );
}

export function MaskPanel() {
  const authoring = useMaskAuthoring();
  if (!authoring?.draft) return null;
  return (
    <MaskEditor
      key={authoring.draft.key}
      mask={authoring.draft.mask}
      pending={authoring.disabled}
      error={authoring.error}
      change={authoring.change}
      save={() => {
        void authoring.save();
      }}
      cancel={authoring.cancel}
    />
  );
}
