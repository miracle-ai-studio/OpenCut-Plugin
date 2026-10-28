// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { preferences } from "../../../shared/locale";
import { textFonts, textInitial } from "../text/fixture";
import { CaptionWorkspace, type CaptionWorkspaceOptions } from "./workspace";
const mocks = vi.hoisted(() => ({
  state: {} as any,
  plan: vi.fn(),
  classic: vi.fn((..._args: unknown[]) => undefined as unknown),
}));
vi.mock("../../context", () => ({ useWorkspace: () => mocks.state, useEditingWorkspace: () => mocks.state }));
vi.mock("./planned-subtitles", () => ({ planEditableSubtitles: mocks.plan }));
vi.mock("../subtitle-classic-ass", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../subtitle-classic-ass")>()),
  planClassicAssSubtitles: mocks.classic,
}));
beforeEach(() => preferences({ language: "en" }));
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});
function fixture() {
  const transaction = {
    project_id: "project",
    expected_revision: "fixed",
    idempotency_key: "original",
    operations: [{ kind: "sequence_put" }],
  };
  const api = {
    connectionIdentity: "original-connection",
    sdk: {
      call: vi.fn(async (name: string) =>
        name === "opencut_text_plan"
          ? { revision: "fixed", initial: textInitial() }
          : { available: false, reasons: [] },
      ),
    },
  };
  const apply = vi.fn(async (..._args: unknown[]) => {}),
    select = vi.fn();
  const head = {
    revision: "fixed",
    project: {
      id: "project",
      fonts: textFonts(),
      sequences: [
        {
          id: "scene",
          tracks: [
            {
              id: "legacy",
              kind: "caption",
              name: "Old captions",
              locked: false,
              elements: [],
            },
          ],
          captions: [{ track_id: "legacy" }],
        },
      ],
    },
  };
  mocks.state = {
    api,
    scene: { head, sequenceId: "scene" },
    jobs: [],
    busy: false,
    controller: {
      getSnapshot: () => mocks.state,
      sync: vi.fn(),
      scene: { applyPlanned: apply, select },
    },
  };
  mocks.plan.mockResolvedValue({
    transaction,
    selection: { track_id: "new-text", element_id: "cue-one" },
    cues: [{}, {}],
    warnings: [],
  });
  const snapshot = { phase: "idle", record: null, error: null, receipt: null };
  const mutateCaption = vi.fn(async () => ({
    cue_count: 1,
    warnings: [],
    transaction: { revision: "saved" },
  }));
  const props = {
    recovery: {
      subscribe: () => () => {},
      getSnapshot: () => snapshot,
      blocked: () => false,
    },
    mutateCaption,
  } as unknown as CaptionWorkspaceOptions;
  return { props, apply, select, transaction, mutateCaption };
}
function importFile() {
  fireEvent.change(screen.getByRole("textbox", { name: /^Subtitle content/ }), {
    target: { value: "1\n00:00:00,000 --> 00:00:01,000\nOriginal subtitle\n" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Import subtitles" }));
}
it("defaults to editable text and applies the original transaction once with 24px real-face defaults", async () => {
  const f = fixture();
  render(<CaptionWorkspace {...f.props} />);
  expect(
    (
      screen.getByRole("combobox", {
        name: /^Caption track/,
      }) as HTMLSelectElement
    ).value,
  ).toBe("__new_subtitle_text_track__");
  importFile();
  await waitFor(() => expect(f.apply).toHaveBeenCalledOnce());
  expect(mocks.plan.mock.calls[0][1]).toMatchObject({
    project_id: "project",
    expected_revision: "fixed",
    sequence_id: "scene",
    track: { kind: "new" },
    source: {
      kind: "file",
      format: "srt",
      content: expect.stringContaining("Original subtitle"),
    },
    style: {
      content: "",
      font: { kind: "locked", font_id: "regular" },
      font_size: { unit: "canvas_pixels", value: 24 },
    },
  });
  expect(f.apply).toHaveBeenCalledWith(f.transaction, "scene");
  expect(f.select).toHaveBeenCalledWith("cue-one");
  expect(f.mutateCaption).not.toHaveBeenCalled();
});
it("preserves legacy caption import recovery when that track is explicitly chosen", async () => {
  const f = fixture();
  render(<CaptionWorkspace {...f.props} />);
  fireEvent.change(screen.getByRole("combobox", { name: /^Caption track/ }), {
    target: { value: "legacy" },
  });
  importFile();
  await waitFor(() => expect(f.mutateCaption).toHaveBeenCalledOnce());
  expect(mocks.plan).not.toHaveBeenCalled();
  expect(f.apply).not.toHaveBeenCalled();
});
function successfulTranscript(revision = "fixed") {
  mocks.state.jobs = [
    {
      job_id: "original-asr-job",
      state: "succeeded",
      kind: "analysis.sequence_transcribe",
      target: { project_id: "project", revision, sequence_id: "scene" },
      result: {
        kind: "analysis.sequence_transcribe",
        data: {
          project_id: "project",
          revision,
          sequence_id: "scene",
          audio: { sample_rate: 48000, frame_count: "48000" },
          transcription: { segments: [{ text: "Original spoken words" }] },
        },
      },
    },
  ];
}
function selectTranscript() {
  fireEvent.change(
    screen.getByRole("combobox", { name: /^Speech recognition result/ }),
    { target: { value: "original-asr-job" } },
  );
}
it("allows an exact-revision successful ASR task to generate into the default new Text track once", async () => {
  const f = fixture();
  successfulTranscript();
  render(<CaptionWorkspace {...f.props} />);
  selectTranscript();
  const generate = screen.getByRole("button", {
    name: "Generate captions",
  }) as HTMLButtonElement;
  expect(generate.disabled).toBe(false);
  fireEvent.change(
    screen.getByRole("spinbutton", { name: /^Words per caption/ }),
    { target: { value: "4" } },
  );
  fireEvent.click(generate);
  fireEvent.click(generate);
  await waitFor(() => expect(f.apply).toHaveBeenCalledOnce());
  expect(mocks.plan).toHaveBeenCalledOnce();
  expect(mocks.plan.mock.calls[0][1]).toMatchObject({
    expected_revision: "fixed",
    track: { kind: "new" },
    source: {
      kind: "transcript",
      analysis_job_id: "original-asr-job",
      words_per_caption: 4,
    },
  });
  expect(f.apply).toHaveBeenCalledWith(f.transaction, "scene");
  expect(f.mutateCaption).not.toHaveBeenCalled();
});
it.each(["stale revision", "locked legacy track"])(
  "blocks generation for %s without planning or applying",
  async (condition) => {
    const f = fixture();
    successfulTranscript(condition === "stale revision" ? "older" : "fixed");
    if (condition === "locked legacy track")
      mocks.state.scene.head.project.sequences[0].tracks[0].locked = true;
    render(<CaptionWorkspace {...f.props} />);
    selectTranscript();
    if (condition === "locked legacy track")
      fireEvent.change(
        screen.getByRole("combobox", { name: /^Caption track/ }),
        { target: { value: "legacy" } },
      );
    const generate = screen.getByRole("button", {
      name: "Generate captions",
    }) as HTMLButtonElement;
    expect(generate.disabled).toBe(true);
    fireEvent.click(generate);
    expect(mocks.plan).not.toHaveBeenCalled();
    expect(f.apply).not.toHaveBeenCalled();
    expect(f.mutateCaption).not.toHaveBeenCalled();
  },
);

function classicPlan(request: any, patch: Record<string, unknown> = {}) {
  return {
    
    revision: request.expected_revision,
    width: 320,
    height: 180,
    font_requirements: [],
    cues: [{ layer: { elementId: "classic-cue" } }],
    warnings: ["Ignored ASS event effects in 1 subtitle cue(s)."],
    skipped_cue_count: 0,
    transaction: {
      project_id: request.project_id,
      expected_revision: request.expected_revision,
      idempotency_key: request.idempotency_key,
      operations: [
        { type: "sequence_put", sequence: { id: request.sequence_id } },
      ],
    },
    selection: { track_id: request.track.track_id, element_id: "classic-cue" },
    ...patch,
  };
}
function importClassicAss() {
  fireEvent.change(screen.getByRole("combobox", { name: /^Format/ }), {
    target: { value: "ass" },
  });
  fireEvent.change(screen.getByRole("textbox", { name: /^Subtitle content/ }), {
    target: { value: "original ASS source" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Import subtitles" }));
}
it("reviews Classic warnings before committing the exact original transaction once", async () => {
  const f = fixture();
  mocks.classic.mockImplementation(async (_api, request) =>
    classicPlan(request),
  );
  render(<CaptionWorkspace {...f.props} />);
  importClassicAss();
  await screen.findByRole("button", { name: "Save reviewed subtitles" });
  expect(
    screen.getByText("Ignored ASS event effects in 1 subtitle cue(s)."),
  ).toBeTruthy();
  expect(f.apply).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("button", { name: "Save reviewed subtitles" }),
  );
  await waitFor(() => expect(f.apply).toHaveBeenCalledOnce());
  const request = mocks.classic.mock.calls[0][1];
  expect(f.apply.mock.calls[0][0]).toEqual(classicPlan(request).transaction);
  expect(f.apply.mock.calls[0][1]).toBe("scene");
});
it.each(["API instance", "connection identity", "scene", "revision"])(
  "synchronously rejects an old review after %s changes before React effects",
  async (change) => {
    const f = fixture();
    mocks.classic.mockImplementation(async (_api, request) =>
      classicPlan(request),
    );
    render(<CaptionWorkspace {...f.props} />);
    importClassicAss();
    const save = await screen.findByRole("button", {
      name: "Save reviewed subtitles",
    });
    if (change === "API instance") mocks.state.api = { ...mocks.state.api };
    else if (change === "connection identity")
      mocks.state.api.connectionIdentity = "different-connection";
    else if (change === "scene") mocks.state.scene.sequenceId = "different-scene";
    else mocks.state.scene.head.revision = "new-head";
    fireEvent.click(save);
    if (change === "revision") {
      await screen.findByText(
        "The subtitle review changed. Import the original file again.",
      );
    } else {
      // Changing connection or scene retires the old form and its notices.
      await waitFor(() => expect(save.isConnected).toBe(false));
    }
    expect(f.apply).not.toHaveBeenCalled();
  },
);
it.each([false, true])(
  "displays source warnings and skipped cues without a transaction (unresolved fonts: %s)",
  async (unresolved) => {
    const f = fixture();
    mocks.classic.mockImplementation(async (_api, request) =>
      classicPlan(request, {
        cues: [],
        transaction: null,
        selection: null,
        skipped_cue_count: 2,
        font_requirements: unresolved
          ? [
              {
                family: "Missing",
                weight: "normal",
                style: "normal",
                resolved_font_id: null,
              },
            ]
          : [],
      }),
    );
    render(<CaptionWorkspace {...f.props} />);
    importClassicAss();
    const review = await screen.findByLabelText("ASS import review");
    expect(review.textContent).toContain("2 invalid or empty cues skipped");
    expect(review.textContent).toContain("Ignored ASS event effects");
    expect(
      screen.queryByRole("button", { name: "Save reviewed subtitles" }),
    ).toBeNull();
    expect(f.apply).not.toHaveBeenCalled();
    if (unresolved)
      expect(
        screen.getByText(
          "Map each missing ASS font to an imported face of the same weight and style, then import again.",
        ),
      ).toBeTruthy();
    else {
      expect(
        screen.getByText(
          "No valid subtitle cues were found. Review the skipped cues and ignored features.",
        ),
      ).toBeTruthy();
      expect(
        screen.queryByText(
          "Map each missing ASS font to an imported face of the same weight and style, then import again.",
        ),
      ).toBeNull();
    }
  },
);
