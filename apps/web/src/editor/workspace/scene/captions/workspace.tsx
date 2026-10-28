import { ErrorNotice } from "../../../shared/error-notice";
import { Select } from "../../../shared/controls";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { jobResult } from "../../../../lib/editor-sdk";
import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import { useEditingWorkspace as useWorkspace } from "../../context";
import type { EditorApi } from "../../api";
import type { ClassicAssRequest } from "../subtitle-classic-ass";
import { Button, Field, Panel } from "../../../shared/ui";
import { downloadBlob } from "../../../shared/download";
import { t, useLocale } from "../../../shared/locale";
import { editRecoveryBlocked } from "../edit-recovery";
import { captionRecoveryView } from "./recovery";
import type { EditRecovery } from "../edit-recovery";
import type {
  CaptionContext,
  CaptionReceipt,
} from "./recovery";
import {
  clearTranscriptionRequest,
  readTranscriptionRequest,
  writeTranscriptionRequest,
} from "./submission";
import type { TranscriptionRequest } from "./submission";
import { planEditableSubtitles } from "./planned-subtitles";
import type { SubtitleTextRequest } from "./planned-subtitles";
import {
  planClassicAssSubtitles,
  ClassicAssImportOptions,
  type ClassicAssPlan,
  type ClassicAssImportMode,
} from "../subtitle-classic-ass";
import { planAssSubtitles, assFontKey } from "./planned-ass";
import type { AssPlan, AssRequest } from "./planned-ass";
import "./workspace.css";
const NEW_TEXT_TRACK = "__new_subtitle_text_track__";
export type CaptionWorkspaceOptions = {
  recovery: EditRecovery;
  mutateCaption(context: CaptionContext): Promise<CaptionReceipt>;
};
type Capability =
  ToolOutput<"opencut_analysis_sequence_transcribe_capability">;
type Style = ToolInput<"opencut_subtitle_generate">["style"];
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
type AssReviewScope = {
  project_id: string;
  expected_revision: string;
  sequence_id: string;
};
export type FrozenClassicAssReview = {
  plan: ClassicAssPlan;
  api: EditorApi;
  connectionIdentity: EditorApi["connectionIdentity"];
  scope: AssReviewScope;
  request: ClassicAssRequest;
  trackChoice: string;
  aliases: string;
  mode: ClassicAssImportMode;
  format: "srt" | "ass";
};
const aliasesIdentity = (aliases: Record<string, string>) =>
  JSON.stringify(
    Object.entries(aliases).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
  );
export type AssReviewLiveContext = {
  api: EditorApi | null;
  formApi: EditorApi | null;
  connectionIdentity: EditorApi["connectionIdentity"] | undefined;
  formConnectionIdentity: EditorApi["connectionIdentity"] | undefined;
  historical: boolean;
  projectId?: string;
  revision?: string;
  sequenceId?: string;
  content: string;
  format: "srt" | "ass";
  trackChoice: string;
  mode: ClassicAssImportMode;
  aliases: Record<string, string>;
};
export function assertAssReviewContext(
  review: Omit<FrozenClassicAssReview, "plan">,
  live: AssReviewLiveContext,
) {
  if (
    live.api !== review.api ||
    live.formApi !== review.api ||
    live.connectionIdentity !== review.connectionIdentity ||
    live.formConnectionIdentity !== review.connectionIdentity ||
    live.historical ||
    live.projectId !== review.scope.project_id ||
    live.revision !== review.scope.expected_revision ||
    live.sequenceId !== review.scope.sequence_id ||
    live.content !== review.request.content ||
    live.format !== review.format ||
    live.trackChoice !== review.trackChoice ||
    live.mode !== review.mode ||
    aliasesIdentity(live.aliases) !== review.aliases
  )
    throw new Error(
      "The subtitle review changed. Import the original file again.",
    );
}

export function CaptionWorkspace({
  recovery,
  mutateCaption,
}: CaptionWorkspaceOptions) {
  useLocale();
  const { api, scene: editorState, jobs, busy, controller } = useWorkspace();
  const restored = captionRecoveryView(useSyncExternalStore(recovery.subscribe, recovery.getSnapshot));
  const [capability, setCapability] = useState<Capability | null>(null),
    [diagnostic, setDiagnostic] = useState<string | null>(null);
  const [language, setLanguage] = useState("auto"),
    [words, setWords] = useState(3),
    [fontId, setFontId] = useState("");
  const [fontSize, setFontSize] = useState(24),
    [trackId, setTrackId] = useState(NEW_TEXT_TRACK),
    [jobId, setJobId] = useState("");
  const [format, setFormat] = useState<"srt" | "ass">("srt"),
    [content, setContent] = useState("");
  const [pending, setPending] = useState(false),
    [notice, setNotice] = useState<string | null>(null),
    [error, setError] = useState<string | null>(null);
  const [assMode, setAssMode] = useState<ClassicAssImportMode>("classic");
  const [classicAssReview, setClassicAssReview] =
    useState<FrozenClassicAssReview | null>(null);
  const [assRequirements, setAssRequirements] = useState<
    AssPlan["font_requirements"]
  >([]);
  const [assAliases, setAssAliases] = useState<Record<string, string>>({});
  const assInputs = useRef({
    api,
    connectionIdentity: api?.connectionIdentity,
    content,
    format,
    trackId,
    aliases: assAliases,
    mode: assMode,
  });
  assInputs.current = {
    api,
    connectionIdentity: api?.connectionIdentity,
    content,
    format,
    trackId,
    aliases: assAliases,
    mode: assMode,
  };
  const [original, setOriginal] = useState<TranscriptionRequest | null>(null);
  const epoch = useRef(0),
    writing = useRef(false),
    measuring = useRef<AbortController | null>(null);
  const snapshot = editorState?.historical ?? editorState?.head,
    scene = snapshot?.project.sequences.find((s) => s.id === editorState?.sequenceId);
  const tracks =
      scene?.tracks.filter(
        (track) => track.kind === "caption" || track.kind === "text",
      ) ?? [],
    fonts = snapshot?.project.fonts ?? [];
  const selectedTrack = tracks.find((track) => track.id === trackId);
  const editableTarget =
    trackId === NEW_TEXT_TRACK || selectedTrack?.kind === "text";
  const validTarget =
    trackId === NEW_TEXT_TRACK || (!!selectedTrack && !selectedTrack.locked);
  const exportable =
    selectedTrack?.kind === "text"
      ? selectedTrack.elements.some(
          (element) => element.content.kind === "text",
        )
      : scene?.captions.some((cue) => cue.track_id === selectedTrack?.id);
  const fixedJobs = jobs.filter(
    (job) =>
      job.kind === "analysis.sequence_transcribe" &&
      job.target?.sequence_id === scene?.id,
  );
  const selectedJob = fixedJobs.find((job) => job.job_id === jobId);
  let ready = false;
  if (selectedJob?.state === "succeeded" && !selectedJob.result_unavailable) {
    try {
      const result = jobResult(
        selectedJob,
        "analysis.sequence_transcribe",
      );
      ready =
        result.audio != null && result.project_id === snapshot?.project.id &&
        result.revision === snapshot?.revision &&
        result.sequence_id === scene?.id;
    } catch {
      ready = false;
    }
  }
  const otherRecovery =
    !!editorState?.recovery?.record ||
    !!editorState?.recovery?.error ||
    ["reading", "staging", "applying"].includes(editorState?.recovery?.phase ?? "") ||
    editRecoveryBlocked(editorState?.recovery);
  const disabled =
    !api ||
    !snapshot ||
    (!editorState || !scene) ||
    !!editorState?.historical ||
    busy ||
    pending ||
    recovery.blocked() ||
    otherRecovery;
  const current = () => {
    const state = controller.getSnapshot();
    if (
      !api ||
      state.api !== api ||
      !state.scene ||
      state.scene.historical ||
      (!editorState || !scene) ||
      state.scene.head.project.id !== snapshot?.project.id ||
      state.scene.head.revision !== snapshot?.revision ||
      state.scene.sequenceId !== scene.id
    )
      throw new Error("The caption target changed.");
    return {
      project_id: state.scene.head.project.id,
      expected_revision: state.scene.head.revision,
      sequence_id: state.scene.sequenceId,
    };
  };
  useEffect(() => {
    epoch.current++;
    measuring.current?.abort();
    setNotice(null);
    setError(null);
    setJobId("");
    setContent("");
    setOriginal(null);
    setPending(false);
    if (!api?.connectionIdentity || (!editorState || !scene)) return;
    try {
      setOriginal(
        readTranscriptionRequest(
          api.connectionIdentity,
          editorState.head.project.id,
          editorState.sequenceId,
        ),
      );
    } catch (e) {
      setError(message(e));
    }
  }, [api, api?.connectionIdentity, editorState?.head.project.id, editorState?.sequenceId]);
  useEffect(
    () => () => {
      measuring.current?.abort();
    },
    [],
  );
  useEffect(() => {
    if (
      trackId !== NEW_TEXT_TRACK &&
      !tracks.some((track) => track.id === trackId)
    )
      setTrackId(
        tracks.find((track) => track.kind === "text" && !track.locked)?.id ??
          NEW_TEXT_TRACK,
      );
  }, [tracks, trackId]);
  useEffect(() => {
    if (!fonts.some((font) => font.id === fontId))
      setFontId(fonts[0]?.id ?? "");
  }, [fonts, fontId]);
  useEffect(() => {
    const cancel = new AbortController();
    setCapability(null);
    setDiagnostic(null);
    if (api)
      void api.sdk
        .call(
          "opencut_analysis_sequence_transcribe_capability",
          {},
          { signal: cancel.signal },
        )
        .then((value) => {
          if (!cancel.signal.aborted) setCapability(value);
        })
        .catch((e) => {
          if (!cancel.signal.aborted) setDiagnostic(message(e));
        });
    return () => cancel.abort();
  }, [api]);
  async function run(action: (active: () => boolean) => Promise<void>) {
    if (writing.current) return;
    const serial = epoch.current;
    writing.current = true;
    setPending(true);
    setError(null);
    setNotice(null);
    try {
      await action(() => serial === epoch.current);
    } catch (e) {
      if (serial === epoch.current) setError(message(e));
    } finally {
      writing.current = false;
      if (serial === epoch.current) setPending(false);
    }
  }
  function style(): Style {
    if (
      !fonts.some((font) => font.id === fontId) ||
      !Number.isFinite(fontSize) ||
      fontSize <= 0 ||
      fontSize > 500
    )
      throw new Error(
        "Select a pinned project font and a size between 1 and 500 px.",
      );
    return {
      font: { kind: "locked", font_id: fontId },
      font_size: { unit: "canvas_pixels", value: fontSize },
      color: { encoding: "srgb", red: 1, green: 1, blue: 1, alpha: 1 },
      background_color: null,
      max_width_px: null,
      line_height_px: null,
      text_align: "center",
      layout: "pinned_caption_v4",
    };
  }
  const createTrack = () =>
    run(async (active) => {
      const scope = current();
      await controller.scene.write("Add subtitle text track", [
        {
          type: "track_put",
          sequence_id: scope.sequence_id,
          track: {
            id: crypto.randomUUID(),
            name: "Editable subtitles",
            kind: "text",
            enabled: true,
            muted: false,
            locked: false,
            opacity: 1,
            gain: { unit: "decibels", value: 0 },
            elements: [],
            channels: [],
          },
        },
      ]);
      if (!active()) return;
      setNotice(
        "Subtitle text track created. Transcribe this saved revision next.",
      );
    });
  const transcribe = (retry: boolean) =>
    run(async (active) => {
      if (!api?.connectionIdentity)
        throw new Error("Authenticate the connection first.");
      const scope = current();
      if (!validTarget)
        throw new Error(
          "Choose an unlocked subtitle track before transcribing.",
        );
      if (!capability?.available)
        throw new Error(
          capability?.unavailable_reason ??
            "Local transcription is unavailable.",
        );
      const request =
        retry && original
          ? original
          : {
              project_id: scope.project_id,
              revision: scope.expected_revision,
              sequence_id: scope.sequence_id,
              language,
              idempotency_key: crypto.randomUUID(),
            };
      writeTranscriptionRequest(api.connectionIdentity, request);
      setOriginal(request);
      const accepted = await api.sdk.call(
        "opencut_analysis_sequence_transcribe",
        request,
      );
      if (
        accepted.kind !== "analysis.sequence_transcribe" ||
        accepted.target?.project_id !== request.project_id ||
        accepted.target?.revision !== request.revision ||
        accepted.target?.sequence_id !== request.sequence_id
      )
        throw new Error(
          "Accepted transcription differs from the original scene.",
        );
      clearTranscriptionRequest(api.connectionIdentity, request);
      if (!active()) return;
      setOriginal(null);
      setJobId(accepted.job_id);
      await controller.sync();
      if (!active()) return;
      setNotice(
        "Local transcription queued. The task list retains progress and cancellation.",
      );
    });
  const saveEditable = async (
    scope: ReturnType<typeof current>,
    source: SubtitleTextRequest["source"],
    active: () => boolean,
  ) => {
    if (!api || !validTarget)
      throw new Error("Choose an unlocked subtitle track.");
    const font = fonts.find((font) => font.id === fontId);
    if (
      !font ||
      ![400, 700].includes(font.weight) ||
      !Number.isFinite(fontSize) ||
      fontSize <= 0 ||
      fontSize > 500
    )
      throw new Error(
        "Select a pinned project font and a size between 1 and 500 px.",
      );
    measuring.current?.abort();
    const abort = new AbortController();
    measuring.current = abort;
    const defaults = await api.sdk.call(
      "opencut_text_plan",
      {
        ...scope,
        idempotency_key: crypto.randomUUID(),
        command: {
          kind: "defaults",
          font_id: fontId,
          start: { kind: "exact", time: { ticks: "0", timebase: 120000 } },
        },
      },
      { signal: abort.signal },
    );
    if (!defaults.initial || defaults.revision !== scope.expected_revision)
      throw new Error("The subtitle plan differs from the fixed revision.");
    const text = structuredClone(defaults.initial.text);
    text.content = "";
    text.font_size = { unit: "canvas_pixels", value: fontSize };
    const request: SubtitleTextRequest = {
      ...scope,
      idempotency_key: crypto.randomUUID(),
      track:
        selectedTrack?.kind === "text"
          ? { kind: "existing", track_id: selectedTrack.id }
          : { kind: "new", track_id: crypto.randomUUID() },
      source,
      style: text,
      max_width_px: null,
    };
    const plan = await planEditableSubtitles(api, request, abort.signal);
    if (!active() || current().expected_revision !== scope.expected_revision)
      throw new Error("The caption target changed.");
    if (!plan.transaction || !plan.selection)
      throw new Error("The subtitle plan differs from the fixed revision.");
    // Original public transaction is staged by the existing durable recovery port.
    // A lost acknowledgement blocks all new operations; never re-transcribe/rekey.
    await controller.scene.applyPlanned(plan.transaction, scope.sequence_id);
    if (!active()) return;
    controller.scene.select(plan.selection.element_id);
    setTrackId(plan.selection.track_id);
    setNotice(
      t("{count} editable subtitles saved. {warnings}", {
        count: plan.cues.length,
        warnings: plan.warnings.join(" "),
      }),
    );
  };
  const generate = () =>
    run(async (active) => {
      const scope = current();
      if (!ready || !selectedJob || !validTarget)
        throw new Error(
          "Select a successful transcript from this exact saved revision and an unlocked caption track.",
        );
      if (editableTarget) {
        await saveEditable(
          scope,
          {
            kind: "transcript",
            analysis_job_id: selectedJob.job_id,
            words_per_caption: words,
          },
          active,
        );
        return;
      }
      const receipt = await mutateCaption({
        sequence_id: scope.sequence_id,
        intent: {
          kind: "subtitle_generate",
          request: {
            ...scope,
            track_id: trackId,
            analysis_job_id: selectedJob.job_id,
            idempotency_key: crypto.randomUUID(),
            words_per_caption: words,
            style: style(),
          },
        },
      });
      if (!active()) return;
      setNotice(
        receipt.cue_count + " captions saved. " + receipt.warnings.join(" "),
      );
    });
  useEffect(() => {
    measuring.current?.abort();
    setAssRequirements([]);
    setAssAliases({});
    setClassicAssReview(null);
  }, [
    content,
    format,
    trackId,
    snapshot?.project.id,
    snapshot?.revision,
    scene?.id,
  ]);
  const requireAssReviewContext = (
    review: Omit<FrozenClassicAssReview, "plan">,
  ) => {
    const state = controller.getSnapshot(),
      form = assInputs.current,
      head = state.scene?.head;
    assertAssReviewContext(review, {
      api: state.api,
      formApi: form.api,
      connectionIdentity: state.api?.connectionIdentity,
      formConnectionIdentity: form.connectionIdentity,
      historical: !state.scene || !!state.scene.historical,
      projectId: head?.project.id,
      revision: head?.revision,
      sequenceId: state.scene?.sequenceId,
      content: form.content,
      format: form.format,
      trackChoice: form.trackId,
      mode: form.mode,
      aliases: form.aliases,
    });
  };
  const saveReviewedAss = () =>
    run(async (active) => {
      const review = classicAssReview;
      if (!review?.plan.transaction || !review.plan.selection)
        throw new Error(
          "Prepare and review the subtitle import before saving.",
        );
      requireAssReviewContext(review);
      const planned = review.plan,
        transaction = planned.transaction!,
        selection = planned.selection!;
      if (
        planned.revision !== review.scope.expected_revision ||
        transaction.project_id !== review.scope.project_id ||
        transaction.expected_revision !== review.scope.expected_revision ||
        transaction.idempotency_key !== review.request.idempotency_key ||
        selection.track_id !== review.request.track.track_id ||
        transaction.operations.length !== 1 ||
        transaction.operations.some(
          (op) =>
            op.type !== "sequence_put" ||
            op.sequence.id !== review.scope.sequence_id,
        )
      )
        throw new Error(
          "The subtitle review changed. Import the original file again.",
        );
      await controller.scene.applyPlanned(transaction, review.scope.sequence_id);
      if (!active()) return;
      controller.scene.select(selection.element_id);
      setTrackId(selection.track_id);
      setClassicAssReview(null);
      setContent("");
      setNotice(
        t("{count} editable subtitles saved. {warnings}", {
          count: planned.cues.length,
          warnings: planned.warnings.join(" "),
        }),
      );
    });
  const saveAss = async (
    scope: ReturnType<typeof current>,
    active: () => boolean,
  ) => {
    if (!api || !validTarget)
      throw new Error("Choose an unlocked subtitle track.");
    measuring.current?.abort();
    const abort = new AbortController();
    measuring.current = abort;
    const request: AssRequest = {
      ...scope,
      idempotency_key: crypto.randomUUID(),
      track:
        selectedTrack?.kind === "text"
          ? { kind: "existing", track_id: selectedTrack.id }
          : { kind: "new", track_id: crypto.randomUUID() },
      content,
      font_bindings: assRequirements.flatMap((requirement) => {
        const font_id = assAliases[assFontKey(requirement)];
        return font_id
          ? [
              {
                family: requirement.family,
                weight: requirement.weight,
                style: requirement.style,
                font_id,
              },
            ]
          : [];
      }),
    };
    const frozen: Omit<FrozenClassicAssReview, "plan"> = {
      api,
      connectionIdentity: api.connectionIdentity,
      scope: structuredClone(scope),
      request: structuredClone(request),
      trackChoice: trackId,
      aliases: aliasesIdentity(assAliases),
      mode: assMode,
      format,
    };
    const planned =
      assMode === "classic"
        ? await planClassicAssSubtitles(
            api,
            request,
            abort.signal,
          )
        : await planAssSubtitles(api, request, abort.signal);
    if (!active()) throw new Error("The caption target changed.");
    requireAssReviewContext(frozen);
    setAssRequirements(planned.font_requirements);
    if (assMode === "classic") {
      setClassicAssReview({
        ...frozen,
        plan: structuredClone(planned as ClassicAssPlan),
      });
      if (planned.font_requirements.some((font) => !font.resolved_font_id))
        setNotice(
          t(
            "Map each missing ASS font to an imported face of the same weight and style, then import again.",
          ),
        );
      else if (!planned.cues.length)
        setNotice(
          t(
            "No valid subtitle cues were found. Review the skipped cues and ignored features.",
          ),
        );
      return;
    }
    if (!planned.transaction || !planned.selection) {
      setNotice(
        t(
          "Map each missing ASS font to an imported face of the same weight and style, then import again.",
        ),
      );
      return;
    }
    await controller.scene.applyPlanned(planned.transaction, scope.sequence_id);
    if (!active()) return;
    controller.scene.select(planned.selection.element_id);
    setTrackId(planned.selection.track_id);
    setContent("");
    setNotice(
      t("{count} editable subtitles saved. {warnings}", {
        count: planned.cues.length,
        warnings: planned.warnings.join(" "),
      }),
    );
  };
  useEffect(() => {
    setClassicAssReview(null);
  }, [assAliases]);
  const importFile = () =>
    run(async (active) => {
      const scope = current();
      if (!validTarget) throw new Error("Choose an unlocked subtitle track.");
      if (
        new TextEncoder().encode(content).length > 2 * 1024 * 1024 ||
        !content.trim()
      )
        throw new Error("Provide an SRT or ASS file up to 2 MiB.");
      if (editableTarget && format === "ass") {
        await saveAss(scope, active);
        return;
      }
      if (editableTarget) {
        await saveEditable(scope, { kind: "file", format, content }, active);
        if (active()) setContent("");
        return;
      }
      const receipt = await mutateCaption({
        sequence_id: scope.sequence_id,
        intent: {
          kind: "subtitle_import",
          request: {
            ...scope,
            track_id: trackId,
            idempotency_key: crypto.randomUUID(),
            format,
            content,
            style: style(),
          },
        },
      });
      if (!active()) return;
      setContent("");
      if (!active()) return;
      setNotice(
        receipt.cue_count + " captions imported. " + receipt.warnings.join(" "),
      );
    });
  const exportFile = () =>
    run(async (active) => {
      if (!api || !snapshot || (!editorState || !scene)) return;
      if (!selectedTrack) throw new Error("Choose a subtitle track to export.");
      const scope = {
        project_id: snapshot.project.id,
        revision: snapshot.revision,
        sequence_id: scene.id,
        format,
      };
      const result =
        selectedTrack.kind === "text"
          ? await api.sdk.call("opencut_subtitle_text_export", {
              ...scope,
              track_id: selectedTrack.id,
            })
          : await api.sdk.call("opencut_subtitle_export", {
              ...scope,
              track_id: selectedTrack.id,
            });
      if (
        result.project_id !== snapshot.project.id ||
        result.revision !== snapshot.revision ||
        result.sequence_id !== scene.id
      )
        throw new Error(
          "Subtitle export differs from the selected fixed revision.",
        );
      if (!active()) return;
      downloadBlob(
        new Blob([result.content], {
          type:
            format === "srt"
              ? "application/x-subrip;charset=utf-8"
              : "text/x-ssa;charset=utf-8",
        }),
        "captions." + format,
      );
      setNotice(
        result.cue_count + " captions exported. " + result.losses.join(" "),
      );
    });
  if (!snapshot || (!editorState || !scene)) return null;
  return (
    <Panel title={t("Captions")} className="editor-caption-workspace">
      <div className="caption-diagnostics" role="status">
        <strong>
          {capability?.available
            ? t("Local transcription ready")
            : t("Local transcription unavailable")}
        </strong>
        <span>
          {t(diagnostic ??
            capability?.unavailable_reason ??
            capability?.timing ??
            "Checking local transcription…")}
        </span>
        <small>
          {t(
            "Captions wrap to 80% of the canvas and sit 5% above its bottom edge. Word timing is estimated within speech segments.",
          )}
        </small>
      </div>
      {restored.record && (
        <div className="caption-recovery" role="status">
          <strong>{t("Resolve the original caption edit")}</strong>
          <p>
            {restored.receipt
              ? t("The original receipt is saved. Acknowledge it to continue.")
              : t(
                  "Retry the saved original request without changing its revision or key.",
                )}
          </p>
          <div className="toolbar">
            <Button
              disabled={busy || pending || !!editorState?.historical}
              onClick={() =>
                void run(async () => {
                  await recovery.retry();
                })
              }
            >
              {t("Retry original captions")}
            </Button>
            <Button
              disabled={busy || pending}
              onClick={() =>
                void run(async () => {
                  await recovery.release();
                })
              }
            >
              {t("Acknowledge original result")}
            </Button>
          </div>
          {restored.error && <ErrorNotice error={restored.error} />}
        </div>
      )}
      <div className="caption-fields">
        <Field label={t("Caption track")}>
          <Select
            value={trackId}
            disabled={disabled}
            onChange={(e) => setTrackId(e.target.value)}
          >
            <option value={NEW_TEXT_TRACK}>
              {t("New editable subtitle track")}
            </option>
            {tracks.map((track) => (
              <option key={track.id} value={track.id}>
                {track.name} ·{" "}
                {t(track.kind === "text" ? "Editable text" : "Legacy captions")}
                {track.locked ? " · " + t("locked") : ""}
              </option>
            ))}
          </Select>
        </Field>
        <Button disabled={disabled} onClick={() => void createTrack()}>
          {t("Add subtitle text track")}
        </Button>
        <Field label={t("Pinned font")}>
          <Select
            value={fontId}
            disabled={disabled}
            onChange={(e) => setFontId(e.target.value)}
          >
            <option value="">{t("Import a font first")}</option>
            {fonts.map((font) => (
              <option key={font.id} value={font.id}>
                {font.family} · {font.weight}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("Size (px)")}>
          <input
            type="number"
            min="1"
            max="500"
            value={fontSize}
            disabled={disabled}
            onChange={(e) => setFontSize(Number(e.target.value))}
          />
        </Field>
      </div>
      <p>
        {t(
          "New subtitles are editable text. Select a legacy caption track to keep its existing caption workflow.",
        )}
      </p>
      <div className="caption-columns">
        <section aria-label={t("Automatic captions")}>
          <h3>{t("Automatic captions")}</h3>
          <div className="caption-fields">
            <Field label={t("Speech language")}>
              <Select
                value={language}
                disabled={disabled || !!original}
                onChange={(e) => setLanguage(e.target.value)}
              >
                {(capability?.supported_languages ?? ["auto", "en", "zh"]).map(
                  (code) => (
                    <option key={code} value={code}>
                      {code === "auto"
                        ? t("Detect automatically")
                        : code === "en"
                          ? t("English")
                          : code === "zh"
                            ? t("Chinese")
                            : code}
                    </option>
                  ),
                )}
              </Select>
            </Field>
            <Field label={t("Words per caption")}>
              <input
                type="number"
                min="1"
                max="20"
                value={words}
                disabled={disabled}
                onChange={(e) => setWords(Number(e.target.value))}
              />
            </Field>
          </div>
          <div className="toolbar">
            <Button
              disabled={
                disabled || !capability?.available || !validTarget || !!original
              }
              onClick={() => void transcribe(false)}
            >
              {t("Transcribe saved scene")}
            </Button>
            {original && (
              <Button
                disabled={disabled || !capability?.available}
                onClick={() => void transcribe(true)}
              >
                {t("Retry original transcription")}
              </Button>
            )}
          </div>
          <Field label={t("Scene transcription")}>
            <Select
              value={jobId}
              onChange={(e) => setJobId(e.target.value)}
              disabled={pending}
            >
              <option value="">{t("Choose a saved task")}</option>
              {fixedJobs.map((job, index) => (
                <option key={job.job_id} value={job.job_id}>
                  {t("Transcription {number}", { number: index + 1 })} · {t(job.state)} · {t(job.target?.revision === editorState?.head.revision ? "Current version" : "Earlier version")}
                </option>
              ))}
            </Select>
          </Field>
          {selectedJob && (
            <div className="caption-task" role="status">
              <span>
                {t(selectedJob.state)}{" "}
                {selectedJob.progress?.fraction != null
                  ? Math.round(selectedJob.progress.fraction * 100) + "%"
                  : ""}
              </span>
              {selectedJob.failure && (
                <ErrorNotice error={selectedJob.failure.message} />
              )}
              {selectedJob.state === "succeeded" && !ready && (
                <small>
                  {t(
                    "This transcript belongs to another saved revision. Transcribe the current scene before generating captions.",
                  )}
                </small>
              )}
              {["queued", "running"].includes(selectedJob.state) && (
                <Button
                  disabled={pending}
                  onClick={() =>
                    void run(async (active) => {
                      await api?.sdk.cancelJob(selectedJob.job_id);
                      if (!active()) return;
                      await controller.sync();
                    })
                  }
                >
                  {t("Cancel transcription")}
                </Button>
              )}
            </div>
          )}
          <div className="toolbar">
            <Button
              disabled={
                disabled ||
                !ready ||
                !fontId ||
                !validTarget ||
                !Number.isInteger(words) ||
                words < 1 ||
                words > 20
              }
              onClick={() => void generate()}
            >
              {t("Generate captions")}
            </Button>
            <Button disabled={pending} onClick={() => void controller.sync()}>
              {t("Refresh tasks")}
            </Button>
          </div>
        </section>
        <section aria-label={t("Subtitle files")}>
          <h3>{t("Subtitle files")}</h3>
          <Field label={t("Format")}>
            <Select
              value={format}
              disabled={pending}
              onChange={(e) => setFormat(e.target.value as "srt" | "ass")}
            >
              <option value="srt">SRT</option>
              <option value="ass">ASS</option>
            </Select>
          </Field>
          <Field label={t("Open subtitle file")}>
            <input
              type="file"
              accept=".srt,.ass,text/plain"
              disabled={disabled}
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (!file) return;
                if (file.size > 2 * 1024 * 1024) {
                  setError("Subtitle files must be at most 2 MiB.");
                  return;
                }
                const serial = epoch.current;
                void file
                  .text()
                  .then((text) => {
                    if (serial === epoch.current) {
                      setContent(text);
                      setFormat(
                        file.name.toLowerCase().endsWith(".ass")
                          ? "ass"
                          : "srt",
                      );
                    }
                  })
                  .catch((err) => setError(message(err)));
              }}
            />
          </Field>
          <Field label={t("Subtitle content")}>
            <textarea
              rows={5}
              value={content}
              disabled={disabled}
              onChange={(e) => setContent(e.target.value)}
              placeholder={t("Paste SRT or ASS text")}
            />
          </Field>
          {editableTarget && format === "ass" && (
            <section aria-label={t("ASS font mapping")}>
              <ClassicAssImportOptions
                mode={assMode}
                onModeChange={(mode) => {
                  measuring.current?.abort();
                  setAssMode(mode);
                  setClassicAssReview(null);
                  setAssRequirements([]);
                  setAssAliases({});
                }}
                plan={classicAssReview?.plan ?? null}
                onAcceptLosses={
                  classicAssReview?.plan.transaction &&
                  classicAssReview.plan.selection
                    ? () => {
                        void saveReviewedAss();
                      }
                    : undefined
                }
              />

              {assMode === "strict" && (
                <p>
                  {t(
                    "ASS styles are preserved where supported. Unsupported ASS features are rejected. Only explicit q2 line breaks are currently supported.",
                  )}
                </p>
              )}
              {assRequirements
                .filter((requirement) => !requirement.resolved_font_id)
                .map((requirement) => {
                  const key = assFontKey(requirement);
                  const matches = fonts.filter(
                    (font) =>
                      font.weight ===
                        (requirement.weight === "bold" ? 700 : 400) &&
                      font.style === requirement.style,
                  );
                  return (
                    <Field
                      key={key}
                      label={`${t("Map ASS font")} ${requirement.family} · ${requirement.weight} · ${requirement.style}`}
                    >
                      <Select
                        disabled={disabled}
                        value={assAliases[key] ?? ""}
                        onChange={(event) =>
                          setAssAliases((previous) => ({
                            ...previous,
                            [key]: event.target.value,
                          }))
                        }
                      >
                        <option value="">{t("Choose an imported face")}</option>
                        {matches.map((font) => (
                          <option key={font.id} value={font.id}>
                            {font.family} · {font.weight} · {font.style}
                          </option>
                        ))}
                      </Select>
                      {!matches.length && (
                        <small>
                          {t(
                            "Import a font of this weight and style before continuing.",
                          )}
                        </small>
                      )}
                    </Field>
                  );
                })}
              {Object.values(assAliases).some(Boolean) && (
                <p role="status">
                  {t(
                    "Font aliases change the original ASS font family and may change glyph layout. The saved import will report this substitution.",
                  )}
                </p>
              )}
            </section>
          )}
          <div className="toolbar">
            <Button
              disabled={
                disabled ||
                !content.trim() ||
                (!fontId && !(editableTarget && format === "ass")) ||
                !validTarget
              }
              onClick={() => void importFile()}
            >
              {t("Import subtitles")}
            </Button>
            <Button
              disabled={!api || pending || busy || !exportable}
              onClick={() => void exportFile()}
            >
              {t("Export subtitles")}
            </Button>
          </div>
          <small>
            {t(
              "SRT stores text and timing. Editable ASS imports preserve supported styles; legacy caption imports use your selected style. Export reports font and layout limitations.",
            )}
          </small>
        </section>
      </div>
      {restored.error && !restored.record && (
        <div role="alert">
          <p>{restored.error}</p>
          <Button
            disabled={busy || pending || !api}
            onClick={() =>
              void run(async () => {
                if (api) await recovery.restore(api, snapshot.project.id);
              })
            }
          >
            {t("Inspect original caption recovery")}
          </Button>
        </div>
      )}
      {!fonts.length && (
        <p>{t("Import a project font in Text before creating captions.")}</p>
      )}
      {error && (
        <ErrorNotice error={error} />
      )}
      {notice && (
        <p className="notice" role="status">
          {notice}
        </p>
      )}
    </Panel>
  );
}
