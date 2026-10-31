import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { createContext, useContext, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useEditingWorkspace as useWorkspace } from "../../context";
import type { EditorApi } from "../../api";
import type { Element, Operation } from "../types";
import { editRecoveryBlocked } from "../edit-recovery";
import { useSynthesisFonts } from "../text/font-synthesis-hook";
import type { TextFont } from "../text/types";
import { stackTargetProblem, validateStack } from "./model";
import type { SceneGeometry } from "./types";
import "./scene-stack.css";

type Scope = {
  epoch: number;
  api: EditorApi;
  connection: string | null;
  projectId: string;
  revision: string;
  sequenceId: string;
  selectedId: string | null;
  timeKey: string;
};
export type SceneStackDraft = {
  scope: Scope;
  trackId: string;
  original: Element;
  element: Element;
  geometry: SceneGeometry | null;
  geometryError: string | null;
  activeMaskId: string | null;
};
type Context = {
  fonts: readonly TextFont[];
  fontLoading: boolean;
  fontError: string | null;
  available: boolean;
  pending: boolean;
  disabled: boolean;
  error: string | null;
  draft: SceneStackDraft | null;
  open(id: string): void;
  change(element: Element): void;
  selectMask(id: string | null): void;
  save(): Promise<void>;
  cancel(): void;
  valid(): boolean;
  transact(label: string, operations: Operation[]): Promise<boolean>;
  report(error: unknown): void;
};
const Context = createContext<Context | null>(null);
export const useSceneStack = () => useContext(Context);
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
const NO_FONTS: readonly TextFont[] = [];

export function SceneStackProvider({ children }: { children: ReactNode }) {
  const { api, scene, controller, busy } = useWorkspace();
  const fontSnapshot = scene?.historical ?? scene?.head;
  const resolvedFonts = useSynthesisFonts(
    api,
    fontSnapshot
      ? { projectId: fontSnapshot.project.id, revision: fontSnapshot.revision }
      : null,
    fontSnapshot?.project.fonts ?? NO_FONTS,
  );
  const [available, setAvailable] = useState(false),
    [pending, setPending] = useState(false),
    [error, setError] = useState<string | null>(null),
    [draft, setDraft] = useState<SceneStackDraft | null>(null);
  const current = useRef<SceneStackDraft | null>(null),
    epoch = useRef(0),
    writing = useRef(false),
    reading = useRef<AbortController | null>(null);
  const publish = (value: SceneStackDraft | null) => {
    current.current = value;
    setDraft(value);
  };
  const blocked = () => {
    const state = controller.getSnapshot(),
      view = state.scene;
    return (
      !view ||
      state.busy ||
      !!view.historical ||
      !!view.gainDraft ||
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
  const disabled = busy || pending || writing.current || blocked();
  const capture = (): Scope => {
    const state = controller.getSnapshot(),
      view = state.scene;
    if (!state.api || !view || blocked())
      throw new Error(
        "Finish playback or resolve the current edit before editing a stack.",
      );
    return {
      epoch: epoch.current,
      api: state.api,
      connection: state.api.connectionIdentity,
      projectId: view.head.project.id,
      revision: view.head.revision,
      sequenceId: view.sequenceId,
      selectedId: view.selectedId,
      timeKey: JSON.stringify(view.playhead),
    };
  };
  const check = (scope: Scope) => {
    const state = controller.getSnapshot(),
      view = state.scene;
    if (
      !view ||
      scope.epoch !== epoch.current ||
      scope.api !== state.api ||
      scope.connection !== state.api?.connectionIdentity ||
      view.head.project.id !== scope.projectId ||
      view.head.revision !== scope.revision ||
      view.sequenceId !== scope.sequenceId ||
      view.selectedId !== scope.selectedId ||
      JSON.stringify(view.playhead) !== scope.timeKey ||
      blocked()
    )
      throw new Error(
        "The stack target changed. Reopen its controls before editing.",
      );
  };
  const checkDraft = (value: SceneStackDraft) => {
    check(value.scope);
    const view = controller.getSnapshot().scene!,
      track = view.head.project.sequences
        .find((s) => s.id === value.scope.sequenceId)
        ?.tracks.find((t) => t.id === value.trackId),
      element = track?.elements.find((e) => e.id === value.original.id);
    if (
      !track ||
      track.locked ||
      !element ||
      JSON.stringify(element) !== JSON.stringify(value.original)
    )
      throw new Error(
        "The stack target changed. Reopen its controls before editing.",
      );
  };
  const valid = () => {
    if (!current.current || writing.current) return false;
    try {
      checkDraft(current.current);
      return true;
    } catch {
      return false;
    }
  };
  const cancel = () => {
    if (writing.current) return;
    epoch.current++;
    reading.current?.abort();
    publish(null);
    setPending(false);
    setError(null);
  };
  useEffect(() => {
    let live = true;
    setAvailable(false);
    if (api)
      void editorHostCapabilities(api)
        .then((result) => {
          if (live)
            setAvailable(
              result.tools.includes("opencut_scene_geometry") &&
                result.tools.includes("opencut_edit_plan"),
            );
        })
        .catch((cause) => {
          if (live) setError(message(cause));
        });
    return () => {
      live = false;
    };
  }, [api, api?.connectionIdentity]);
  const timeKey = JSON.stringify(scene?.playhead),
    playbackBlocked =
      !!scene &&
      ["loading", "playing", "stalled", "unconfirmed"].includes(
        scene.audioPreview.phase,
      );
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
    playbackBlocked,
  ]);
  useEffect(() => {
    if (!writing.current && blocked() && current.current) cancel();
  }, [
    busy,
    scene?.gainDraft,
    scene?.recovery,
    scene?.recovery,
    scene?.timelineEdit.attempt,
    scene?.clipboard.attempt,
  ]);
  const report = (cause: unknown) => setError(message(cause));
  const open = (id: string) => {
    if (!available || disabled || writing.current) return;
    try {
      epoch.current++;
      const scope = capture(),
        view = controller.getSnapshot().scene!,
        track = view.head.project.sequences
          .find((s) => s.id === scope.sequenceId)
          ?.tracks.find((t) => t.elements.some((e) => e.id === id)),
        element = track?.elements.find((e) => e.id === id);
      if (!track || track.locked || !element)
        throw new Error("Unlock the target track before editing its stack.");
      const issue = stackTargetProblem(element);
      if (issue) throw new Error(issue);
      reading.current?.abort();
      const abort = new AbortController();
      reading.current = abort;
      const value: SceneStackDraft = {
        scope,
        trackId: track.id,
        original: structuredClone(element),
        element: structuredClone(element),
        geometry: null,
        geometryError: null,
        activeMaskId: element.visual?.masks[0]?.id ?? null,
      };
      publish(value);
      setError(null);
      if (!["video", "image", "graphic"].includes(element.content.kind)) return;
      setPending(true);
      void scope.api.sdk
        .call(
          "opencut_scene_geometry",
          {
            project_id: scope.projectId,
            expected_revision: scope.revision,
            sequence_id: scope.sequenceId,
            element_id: id,
            time: structuredClone(view.playhead),
          },
          { signal: abort.signal },
        )
        .then((geometry) => {
          if (abort.signal.aborted) return;
          checkDraft(value);
          if (
            geometry.revision !== scope.revision ||
            geometry.element_id !== id ||
            ![
              geometry.quad.centerX,
              geometry.quad.centerY,
              geometry.quad.width,
              geometry.quad.height,
              geometry.quad.rotationRadians,
              geometry.quad.opacity,
              ...Object.values(geometry.default_geometry),
            ].every(Number.isFinite) ||
            geometry.quad.width <= 0 ||
            geometry.quad.height <= 0 ||
            geometry.default_geometry.width <= 0 ||
            geometry.default_geometry.height <= 0 ||
            geometry.default_geometry.scale < 0 ||
            typeof geometry.quad.flipX !== "boolean" ||
            typeof geometry.quad.flipY !== "boolean"
          )
            throw new Error(
              "Scene geometry differs from the requested fixed revision.",
            );
          publish({ ...value, geometry });
        })
        .catch((cause) => {
          if (!abort.signal.aborted && scope.epoch === epoch.current) {
            try {
              checkDraft(value);
              publish({ ...value, geometryError: message(cause) });
            } catch {
              publish(null);
            }
          }
        })
        .finally(() => {
          if (scope.epoch === epoch.current) setPending(false);
        });
    } catch (cause) {
      report(cause);
    }
  };
  const change = (element: Element) => {
    const value = current.current;
    if (!value || !valid() || element.id !== value.element.id) return;
    publish({ ...value, element: structuredClone(element) });
    setError(null);
  };
  const selectMask = (id: string | null) => {
    const value = current.current;
    if (value && valid()) publish({ ...value, activeMaskId: id });
  };
  const execute = async (
    scope: Scope,
    label: string,
    operations: Operation[],
  ) => {
    check(scope);
    const request = {
      project_id: scope.projectId,
      expected_revision: scope.revision,
      idempotency_key: crypto.randomUUID(),
      label,
      operations: structuredClone(operations),
    };
    // Reuse public validation and the original durable transaction request. No
    // local state reconstruction and no second idempotency key on apply/recovery.
    await scope.api.sdk.call("opencut_edit_plan", request);
    check(scope);
    await controller.scene.applyPlanned(request, scope.sequenceId);
  };
  const transact = async (label: string, operations: Operation[]) => {
    if (writing.current || disabled || !available) return false;
    let scope: Scope;
    try {
      scope = capture();
      writing.current = true;
      setPending(true);
      setError(null);
      await execute(scope, label, operations);
      if (scope.epoch === epoch.current) publish(null);
      return true;
    } catch (cause) {
      report(cause);
      return false;
    } finally {
      writing.current = false;
      setPending(false);
    }
  };
  const save = async () => {
    const value = current.current;
    if (!value || writing.current || disabled) return;
    try {
      checkDraft(value);
      const issue = validateStack(value.element);
      if (issue) throw new Error(issue);
      if (JSON.stringify(value.original) === JSON.stringify(value.element)) {
        cancel();
        return;
      }
      await transact("Edit visual stack", [
        {
          type: "element_put",
          sequence_id: value.scope.sequenceId,
          track_id: value.trackId,
          element: value.element,
        },
      ]);
    } catch (cause) {
      report(cause);
    }
  };
  return (
    <Context.Provider
      value={{
        fonts: resolvedFonts.fonts,
        fontLoading: resolvedFonts.loading,
        fontError: resolvedFonts.error,
        available,
        pending,
        disabled,
        error,
        draft,
        open,
        change,
        selectMask,
        save,
        cancel,
        valid,
        transact,
        report,
      }}
    >
      {children}
    </Context.Provider>
  );
}
