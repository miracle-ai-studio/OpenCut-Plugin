// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { WorkspaceProvider } from "../context";
import { Product } from "./product";
import { setup } from "./fixture";
import { BookmarkEditor } from "./bookmarks";
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});
it("keeps Properties visible after a failed direct Undo and disables history writes during a local draft", async () => {
  const f = await setup();
  try {
    await f.controller.scene.bookmark({ kind: "put", bookmark: { id: "b", sequence_id: "scene", time: { ticks: "0", timebase: 1 }, note: "History action" } });
    f.controller.scene.select("image");
    const undo = vi.spyOn(f.controller.scene, "planUndo").mockRejectedValue(Error("Confirm or cancel the property draft before starting another edit."));
    render(<WorkspaceProvider controller={f.controller}><Product /></WorkspaceProvider>);
    fireEvent.click(screen.getByRole("button", { name: "Undo last edit" }));
    await waitFor(() => expect(f.controller.getSnapshot().error).toContain("property draft"));
    expect(screen.getByRole("tab", { name: "Properties", selected: true })).toBeTruthy();
    expect(f.controller.getSnapshot().scene!.selectedId).toBe("image");
    act(() => f.controller.scene.propertyDraft("guard", true));
    expect((screen.getByRole("button", { name: "Undo last edit" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Undo last edit" }));
    expect(undo).toHaveBeenCalledOnce();
    act(() => f.controller.scene.propertyDraft("guard", false));
    expect((screen.getByRole("button", { name: "Undo last edit" }) as HTMLButtonElement).disabled).toBe(false);
  } finally { f.controller.dispose(); }
});
it("opens history from the header view request and focuses its tab without a project edit",async()=>{
 const f=await setup();try{
  const view=render(<WorkspaceProvider controller={f.controller}><Product historyRequest={0}/></WorkspaceProvider>);
  const before=structuredClone(f.current());view.rerender(<WorkspaceProvider controller={f.controller}><Product historyRequest={1}/></WorkspaceProvider>);
  const tab=screen.getByRole("tab",{name:"History",selected:true});expect(document.activeElement).toBe(tab);expect(f.current()).toEqual(before);
 }finally{f.controller.dispose();}
});
it("preserves resource search across tool changes and routes clip selection back to its properties without an edit", async () => {
  const { controller, api, current } = await setup();
  try {
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    const resources=within(screen.getByLabelText("Editing tools"));
    const inspector=within(screen.getByLabelText("Inspector view"));
    const inspectorBody=within(screen.getByLabelText("Inspector"));
    const categories=within(screen.getByLabelText("Resource categories"));
    const timeline=within(screen.getByLabelText("Timeline",{selector:"section"}));
    const sourceRevision = current().revision;
    const search = screen.getByPlaceholderText("Search media") as HTMLInputElement;
    fireEvent.change(search, { target: { value: "Image" } });
    fireEvent.keyDown(resources.getByRole("tab", { name: "Media" }), { key: "ArrowRight" });
    expect(resources.getByRole("tab", { name: "Audio", selected: true })).toBe(document.activeElement);
    fireEvent.click(resources.getByRole("tab", { name: "Graphics" }));
    expect(categories.getByRole("button", { name: "Flags", pressed: false })).toBeTruthy();
    expect(categories.getByRole("button", { name: "Brands", pressed: false })).toBeTruthy();
    fireEvent.click(resources.getByRole("tab", { name: "Media" }));
    expect(screen.getByPlaceholderText("Search media")).toBe(search);
    expect(search.value).toBe("Image");
    fireEvent.click(inspector.getByRole("tab", { name: "History" }));
    expect(inspectorBody.getByRole("region", { name: "History" })).toBeTruthy();
    fireEvent.click(timeline.getByRole("button", { name: "Whole image · Image" }));
    expect(inspector.getByRole("tab", { name: "Properties", selected: true })).toBeTruthy();
    fireEvent.click(inspector.getByRole("tab", { name: "History" }));
    fireEvent.click(timeline.getByRole("button", { name: "Whole image · Image" }));
    expect(inspector.getByRole("tab", { name: "Properties", selected: true })).toBeTruthy();
    expect(inspectorBody.getByRole("region", { name: "History", hidden: true })).toBeTruthy();
    expect(current().revision).toBe(sourceRevision);
    expect(vi.mocked(api.sdk.call).mock.calls.filter(([name]) => name === "opencut_edit_apply")).toHaveLength(0);
  } finally { controller.dispose(); }
});
it("opens fixed source bytes in the central player and retains the timeline view on return without an edit", async () => {
  const {controller,api,current}=await setup();
  try {
    const asset=current().project.assets[0];
    vi.spyOn(api.scene,"resource").mockResolvedValue({project_id:current().project.id,revision:current().revision,resource_id:asset.id,kind:"asset",version:asset.version,mime_type:"image/png",content_path:"unused"});
    vi.spyOn(api.sdk,"projectResourceUrl").mockReturnValue(`${location.origin}/source.png`);
    const view=render(<WorkspaceProvider controller={controller}><Product/></WorkspaceProvider>);
    const canvas=view.container.querySelector(".workbench-player-surface"), at=controller.scene.currentPlayhead(), selected=controller.getSnapshot().scene!.selectedId;
    fireEvent.click(screen.getByRole("button",{name:"Preview source Image"}));
    await screen.findByRole("img",{name:"Image"});
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(view.container.querySelector(".workbench-timeline-player")?.getAttribute("aria-hidden")).toBe("true");
    fireEvent.click(screen.getByRole("button",{name:"Return to timeline"}));
    expect(view.container.querySelector(".workbench-player-surface")).toBe(canvas);
    expect(view.container.querySelector(".workbench-timeline-player")?.getAttribute("aria-hidden")).toBe("false");
    expect(controller.scene.currentPlayhead()).toEqual(at);expect(controller.getSnapshot().scene!.selectedId).toBe(selected);
    expect(vi.mocked(api.sdk.call).mock.calls.filter(([name])=>name==="opencut_edit_apply")).toHaveLength(0);
  } finally {controller.dispose();}
});
it("native file paste captures the selected track and exact playhead, while text input retains clipboard ownership", async () => {
  const { controller, current } = await setup();
  try {
    current().project.sequences[0].tracks.push({ ...structuredClone(current().project.sequences[0].tracks[0]),
      id: "selected-audio", name: "Audio", kind: "audio", elements: [] });
    current().revision = "selected-audio-head";
    await controller.scene.refresh();
    controller.scene.select("selected-audio"); controller.scene.seek({ ticks: "1", timebase: 3 });
    const importing = vi.spyOn(controller.scene.media, "importFiles").mockRejectedValue(new Error("Synthetic upload interruption"));
    const view = render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    const file = new File(["wav"], "sound.wav", { type: "audio/wav" });
    const data = { files: [file], getData: () => "" };
    fireEvent.paste(view.container.querySelector(".editor-timeline-panel")!, { clipboardData: data });
    await waitFor(() => expect(importing).toHaveBeenCalledOnce());
    expect(importing).toHaveBeenCalledWith([file], undefined, {
      scope: { projectId: current().project.id, revision: "selected-audio-head", sequenceId: "scene" },
      trackId: "selected-audio", at: { ticks: "1", timebase: 3 },
    });
    fireEvent.paste(screen.getByLabelText("Playhead (seconds)"), { clipboardData: data });
    expect(importing).toHaveBeenCalledOnce();
  } finally { controller.dispose(); }
});

it("keeps a focused or dirty seek intention through confirmed consumer progress and submits that exact time", async () => {
  const { controller } = await setup();
  try {
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    const input = screen.getByLabelText("Playhead (seconds)") as HTMLInputElement;
    // The callback is the same Session boundary used for Rust-confirmed Time;
    // this component test does not claim an actual audio device observation.
    act(() => controller.scene["presentAudio"]({ ticks: "1", timebase: 2 }));
    expect(input.value).toBe("0.500000");
    fireEvent.focus(input);
    act(() => controller.scene["presentAudio"]({ ticks: "3", timebase: 4 }));
    expect(input.value).toBe("0.500000");
    fireEvent.blur(input);
    expect(input.value).toBe("0.750000");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "296.250000" } });
    act(() => controller.scene["presentAudio"]({ ticks: "1", timebase: 1 }));
    expect(input.value).toBe("296.250000");
    fireEvent.blur(input);
    act(() => controller.scene["presentAudio"]({ ticks: "5", timebase: 4 }));
    expect(input.value).toBe("296.250000");
    const seek = vi.spyOn(controller.scene, "seek");
    fireEvent.click(screen.getByRole("button", { name: "Go to time" }));
    expect(seek).toHaveBeenCalledExactlyOnceWith({ ticks: "1185", timebase: 4 });
    act(() => controller.scene["presentAudio"]({ ticks: "297", timebase: 1 }));
    expect(input.value).toBe("297.000000");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "12.345678" } });
    act(() => controller.scene["presentAudio"]({ ticks: "299", timebase: 1 }));
    fireEvent.keyDown(input, { key: "Enter" });
    expect(seek).toHaveBeenLastCalledWith({ ticks: "6172839", timebase: 500000 });
  } finally { controller.dispose(); }
});
it("cancels a seek draft without dispatch and clears it when the scene changes", async () => {
  const { controller, current } = await setup();
  try {
    const next = structuredClone(current().project.sequences[0]);
    next.id = "next-scene"; next.main_track_id = "next-track";
    next.tracks[0].id = "next-track"; next.tracks[0].elements[0].id = "next-image";
    current().project.sequences.push(next);
    current().revision = "second-scene-head";
    await controller.scene.refresh();
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    const input = screen.getByLabelText("Playhead (seconds)") as HTMLInputElement;
    const seek = vi.spyOn(controller.scene, "seek");
    fireEvent.focus(input); fireEvent.change(input, { target: { value: "42" } });
    act(() => controller.scene["presentAudio"]({ ticks: "2", timebase: 1 }));
    fireEvent.keyDown(input, { key: "Escape" });
    expect(input.value).toBe("2.000000"); expect(seek).not.toHaveBeenCalled();
    act(() => controller.scene["presentAudio"]({ ticks: "3", timebase: 1 }));
    expect(input.value).toBe("3.000000");
    fireEvent.change(input, { target: { value: "99" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Scene" }), { target: { value: "next-scene" } });
    expect(input.value).toBe("0.000000");
    act(() => controller.scene["presentAudio"]({ ticks: "1", timebase: 4 }));
    expect(input.value).toBe("0.250000"); expect(seek).not.toHaveBeenCalled();
  } finally { controller.dispose(); }
});
it("explains invalid time input and retires preview when the editing cursor reaches the end", async () => {
  const { controller } = await setup();
  try {
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    fireEvent.click(screen.getByLabelText("Go to time", { selector: "summary" }));
    const input = screen.getByLabelText("Playhead (seconds)");
    const previewSeek = vi.spyOn(controller.scene.audioPreview, "seek");
    fireEvent.change(input, { target: { value: "00:12.00" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByRole("alert").textContent).toContain("up to 6 decimal places");
    expect(previewSeek).not.toHaveBeenCalled();

    fireEvent.change(input, { target: { value: "12" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input.hasAttribute("aria-invalid")).toBe(false);
    expect(previewSeek).toHaveBeenCalledExactlyOnceWith(undefined);
    expect(controller.getSnapshot().scene?.playhead).toEqual({ ticks: "12", timebase: 1 });
  } finally { controller.dispose(); }
});
it("synchronizes the selected Scene URL without changing another project's route or navigation state", async () => {
  const originalUrl = location.href, originalState = history.state;
  const { controller, current } = await setup();
  try {
    const next = structuredClone(current().project.sequences[0]);
    next.id = "next-scene"; next.name = "Next scene";
    next.main_track_id = "next-track"; next.tracks[0].id = "next-track";
    next.tracks[0].elements[0].id = "next-image";
    current().project.sequences.push(next);
    current().revision = "second-scene-route-head";
    await controller.scene.refresh();
    const navigation = { panel: "timeline", position: 7 };
    history.replaceState(navigation, "", "/editor/v4-project?scene=scene&layout=compact#timeline");
    const view = render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    fireEvent.change(screen.getByRole("combobox", { name: "Scene" }), { target: { value: "next-scene" } });
    expect(controller.getSnapshot().scene!.sequenceId).toBe("next-scene");
    expect(location.pathname + location.search + location.hash).toBe("/editor/v4-project?scene=next-scene&layout=compact#timeline");
    expect(history.state).toEqual(navigation);
    expect(new URL(location.href).searchParams.get("scene")).toBe(controller.getSnapshot().scene!.sequenceId);
    view.unmount();
    history.replaceState(navigation, "", "/editor/different-project?scene=other&layout=compact#timeline");
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    fireEvent.change(screen.getByRole("combobox", { name: "Scene" }), { target: { value: "scene" } });
    expect(controller.getSnapshot().scene!.sequenceId).toBe("scene");
    expect(location.pathname + location.search + location.hash).toBe("/editor/different-project?scene=other&layout=compact#timeline");
    expect(history.state).toEqual(navigation);
  } finally {
    cleanup(); controller.dispose(); history.replaceState(originalState, "", originalUrl);
  }
});
it("uses the shared timeline selection for modifier clicks and preserves bookmark selection", async () => {
  const { controller, current } = await setup();
  try {
    const sequence = current().project.sequences[0];
    const second = structuredClone(sequence.tracks[0].elements[0]);
    second.id = "second-image";
    second.name = "Second image";
    sequence.tracks[0].elements.push(second);
    sequence.bookmarks.push({ id: "bookmark", sequence_id: sequence.id, time: { ticks: "0", timebase: 1 }, note: "Scene note" });
    current().revision = "multi-selection-head";
    await controller.scene.refresh();
    render(<WorkspaceProvider controller={controller}><Product /></WorkspaceProvider>);
    const timeline=within(screen.getByLabelText("Timeline",{selector:"section"}));
    const firstButton = timeline.getByRole("button", { name: "Whole image · Image" });
    const secondButton = timeline.getByRole("button", { name: "Second image · Image" });
    fireEvent.click(firstButton);
    fireEvent.click(secondButton, { shiftKey: true });
    expect(controller.getSnapshot().scene!.timelineEdit.selection.map(target => target.element_id)).toEqual(["image", "second-image"]);
    expect(firstButton.getAttribute("aria-pressed")).toBe("true");
    expect(secondButton.getAttribute("aria-pressed")).toBe("true");
    expect(timeline.getByRole("button", { name: "Move selection" }).hasAttribute("disabled")).toBe(false);
    fireEvent.click(firstButton, { ctrlKey: true });
    expect(controller.getSnapshot().scene!.timelineEdit.selection.map(target => target.element_id)).toEqual(["second-image"]);
    fireEvent.click(timeline.getByRole("button", { name: "Bookmark at 00:00.00: Scene note" }));
    expect(controller.getSnapshot().scene!.selectedId).toBe("bookmark");
    expect(controller.getSnapshot().scene!.timelineEdit.selection).toEqual([]);
    expect(timeline.getByRole("button", { name: "Move selection" }).hasAttribute("disabled")).toBe(true);
  } finally { controller.dispose(); }
});
it("provides bookmark CRUD through timeline markers, canvas notes and selective history in the product workspace", async () => {
  const { controller, current, ref } = await setup();
  try {
    render(
      <WorkspaceProvider controller={controller}>
        <Product />
      </WorkspaceProvider>,
    );
    const timeline = () => within(screen.getByLabelText("Timeline",{selector:"section"}));
    const inspector = () => within(screen.getByLabelText("Inspector"));
    const player = within(screen.getByLabelText("Player"));
    const clickReady = async (name: string) => {
      await waitFor(() => expect(controller.getSnapshot().busy).toBe(false));
      const button = await (name === "Add bookmark" ? timeline() : name === "Go to time" ? player : inspector()).findByText(name, { selector: "button" });
      await waitFor(() => expect(button.hasAttribute("disabled")).toBe(false));
      fireEvent.click(button);
    };
    expect(screen.queryByText("Play to preview your timeline")).toBeNull();
    expect(within(screen.getByLabelText("Editing tools")).getByRole("tab", { name: "Media", selected: true })).toBeTruthy();
    await clickReady("Add bookmark");
    fireEvent.change(screen.getByLabelText("Bookmark time (seconds)"), {
      target: { value: "2" },
    });
    fireEvent.change(screen.getByLabelText("Canvas note"), {
      target: { value: "Review this frame" },
    });
    await clickReady("Save bookmark");
    await waitFor(() =>
      expect(current().project.sequences[0].bookmarks).toHaveLength(1),
    );
    const marker = await timeline().findByLabelText("Bookmark at 00:02.00: Review this frame");
    fireEvent.click(marker);
    expect(player.getByRole("region", { name: "Canvas notes" })).toBeTruthy();
    expect(
      player.getByRole("region", { name: "Canvas notes" }).textContent,
    ).toContain("Review this frame");
    await clickReady("Edit bookmark");
    fireEvent.change(screen.getByLabelText("Canvas note"), {
      target: { value: "Updated note" },
    });
    await clickReady("Save bookmark");
    await timeline().findByLabelText("Bookmark at 00:02.00: Updated note");
    fireEvent.click(player.getByLabelText("Go to time",{selector:"summary"}));
    fireEvent.change(player.getByLabelText("Playhead (seconds)"), {
      target: { value: "4" },
    });
    await clickReady("Go to time");
    await clickReady("Move to playhead");
    await timeline().findByLabelText("Bookmark at 00:04.00: Updated note");
    await act(async () => {
      await controller.open(ref());
    });
    fireEvent.click(
      timeline().getByLabelText("Bookmark at 00:04.00: Updated note"),
    );
    await clickReady("Delete bookmark");
    await waitFor(() =>
      expect(current().project.sequences[0].bookmarks).toHaveLength(0),
    );
    await waitFor(() =>
      expect(
        timeline().queryByLabelText("Bookmark at 00:04.00: Updated note"),
      ).toBeNull(),
    );
    fireEvent.click(timeline().getByRole("button", { name: "Undo last edit" }));
    await timeline().findByLabelText("Bookmark at 00:04.00: Updated note");
    await waitFor(() => expect(timeline().getByRole("button", { name: "Redo last edit" }).hasAttribute("disabled")).toBe(false));
    fireEvent.click(timeline().getByRole("button", { name: "Redo last edit" }));
    await waitFor(() =>
      expect(current().project.sequences[0].bookmarks).toHaveLength(0),
    );
  } finally {
    controller.dispose();
  }
}, 10000);
it("editing a note preserves its original exact non-terminating bookmark time and duration", async () => {
  const save = vi.fn().mockResolvedValue({});
  render(
    <BookmarkEditor
      bookmark={{
        id: "b",
        sequence_id: "s",
        time: { ticks: "1", timebase: 3 },
        duration: { ticks: "2", timebase: 3 },
        note: "old",
      }}
      sequenceId="s"
      playhead={{ ticks: "0", timebase: 1 }}
      disabled={false}
      save={save}
      close={() => {}}
    />,
  );
  fireEvent.change(screen.getByLabelText("Canvas note"), {
    target: { value: "new" },
  });
  fireEvent.click(screen.getByText("Save bookmark"));
  await waitFor(() => expect(save).toHaveBeenCalled());
  expect(save.mock.calls[0][0].bookmark.time).toEqual({
    ticks: "1",
    timebase: 3,
  });
  expect(save.mock.calls[0][0].bookmark.duration).toEqual({
    ticks: "2",
    timebase: 3,
  });
});
it("shows canvas notes at inclusive exact endpoints, excludes whitespace and never forces selected notes outside their interval", async () => {
  const { controller, current } = await setup();
  try {
    await controller.scene.bookmark({
      kind: "put",
      bookmark: {
        id: "note",
        sequence_id: "scene",
        time: { ticks: "1", timebase: 3 },
        duration: { ticks: "1", timebase: 6 },
        note: "Exact endpoint",
      },
    });
    await controller.scene.bookmark({
      kind: "put",
      bookmark: {
        id: "blank",
        sequence_id: "scene",
        time: { ticks: "1", timebase: 3 },
        duration: { ticks: "1", timebase: 6 },
        note: "  \n ",
      },
    });
    controller.scene.seek({ ticks: "1", timebase: 3 });
    controller.scene.select("blank");
    render(
      <WorkspaceProvider controller={controller}>
        <Product />
      </WorkspaceProvider>,
    );
    const canvas = screen.getByRole("region", { name: "Canvas notes" });
    expect(canvas.textContent).toContain("Exact endpoint");
    expect(canvas.querySelectorAll(".editor-canvas-note")).toHaveLength(1);
    act(() => controller.scene.seek({ ticks: "1", timebase: 2 }));
    expect(canvas.textContent).toContain("Exact endpoint");
    act(() => {
      controller.scene.select("note");
      controller.scene.seek({ ticks: "501", timebase: 1000 });
    });
    expect(canvas.textContent).not.toContain("Exact endpoint");
    expect(current().project.sequences[0].bookmarks).toHaveLength(2);
  } finally {
    controller.dispose();
  }
});
it("a new note uses the exact playhead when its displayed time is untouched", async () => {
  const save = vi.fn().mockResolvedValue({});
  render(
    <BookmarkEditor
      bookmark={null}
      sequenceId="s"
      playhead={{ ticks: "1", timebase: 3 }}
      disabled={false}
      save={save}
      close={() => {}}
    />,
  );
  fireEvent.change(screen.getByLabelText("Canvas note"), {
    target: { value: "new" },
  });
  fireEvent.click(screen.getByText("Save bookmark"));
  await waitFor(() => expect(save).toHaveBeenCalled());
  expect(save.mock.calls[0][0].bookmark.time).toEqual({
    ticks: "1",
    timebase: 3,
  });
});
