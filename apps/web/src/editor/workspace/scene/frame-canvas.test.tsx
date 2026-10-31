// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { WorkspaceProvider } from "../context";
import { Product } from "./product";
import { setupFrame } from "./frame-fixture";
import * as downloads from "../../shared/download";
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("saves the original scene frame, keeps notes as a separate HUD and removes stale pixels immediately on seek", async () => {
  const f = await setupFrame();
  const save=vi.spyOn(downloads,"saveResponse").mockImplementation(async (_name,fetchResponse)=>{
    expect((await fetchResponse()).headers.get("content-type")).toBe("image/png");
  });
  const create = vi.fn(() => "blob:scene-frame"),
    revoke = vi.fn();
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: create,
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: revoke,
  });
  try {
    f.current().project.sequences[0].bookmarks = [
      {
        id: "note",
        sequence_id: "scene",
        time: { ticks: "0", timebase: 1 },
        duration: { ticks: "2", timebase: 1 },
        note: "Editor-only note",
      },
    ];
    await f.controller.scene.refresh();
    render(
      <WorkspaceProvider controller={f.controller}>
        <Product />
      </WorkspaceProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Save snapshot" }));
    await waitFor(() => expect(f.requests).toHaveLength(1));
    await act(async () => {
      f.complete();
      await f.controller.sync();
    });
    const image = await screen.findByAltText("Rendered scene frame");
    expect(image.getAttribute("src")).toBe("blob:scene-frame");
    expect(save).toHaveBeenCalledOnce();
    expect(
      screen.getByRole("region", { name: "Canvas notes" }).textContent,
    ).toContain("Editor-only note");
    expect(f.requests[0]).not.toHaveProperty("bookmarks");
    await act(async () => f.controller.scene.seek({ ticks: "1", timebase: 3 }));
    expect(screen.queryByAltText("Rendered scene frame")).toBeNull();
    expect(
      screen.getByRole("region", { name: "Canvas notes" }).textContent,
    ).toContain("Editor-only note");
    await waitFor(() =>
      expect(revoke).toHaveBeenCalledWith("blob:scene-frame"),
    );
    expect(screen.queryByRole("button", { name: "Play preview" })).toBeNull();
  } finally {
    cleanup();
    f.controller.dispose();
  }
});

it("previews through the native owner once when the button beats automatic preview, without an export job", async () => {
  const f=await setupFrame();
  vi.spyOn(f.api.scene,"audioPreviewCapability").mockResolvedValue({
    available:true,unavailable_reason:null,profile:"classic-realtime-v31",sample_rates:[44100,48000],
    stream_path:"/api/preview/audio/stream",max_sessions_per_database:4,pcm_stage:"mastered",
    max_block_frames:4096,max_buffered_frames:8192,owner_lease_ms:30000,
    authorization_heartbeat_ms:1000,session_ttl_ms:120000,
  });
  await f.controller.scene.audioPreview.capability();
  const shown=vi.spyOn(f.controller.scene.visualPreview,"showStill").mockResolvedValue(undefined);
  vi.useFakeTimers();
  try {
    render(<WorkspaceProvider controller={f.controller}><Product /></WorkspaceProvider>);
    const button=screen.getByRole("button",{name:"Preview current frame"}) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    await act(async()=>{fireEvent.click(button);});
    expect(shown).toHaveBeenCalledOnce();
    await act(async()=>{vi.advanceTimersByTime(61);});
    expect(shown).toHaveBeenCalledOnce();
    expect(f.requests).toHaveLength(0);
  } finally {cleanup();f.controller.dispose();}
});

it("disables unavailable services with a clear capability reason while the rest of the editor stays usable", async () => {
  const f = await setupFrame();
  try {
    f.setCapability({available:false,unavailable_reason:"GPU worker unavailable."});
    await f.controller.scene.frame.refreshCapability();
    render(
      <WorkspaceProvider controller={f.controller}>
        <Product />
      </WorkspaceProvider>,
    );
    expect(
      screen
        .getByRole("button", { name: "Preview current frame" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(screen.getByText("GPU worker unavailable.")).toBeTruthy();
    expect(
      screen
        .getByRole("button", { name: "Add bookmark" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(f.requests).toHaveLength(0);
  } finally {
    cleanup();
    f.controller.dispose();
  }
});

it("retains owner pixels and redraws the paused picture after a theme change", async () => {
  const f=await setupFrame();const {preferences}=await import('../../shared/locale');
  const bitmap={width:2,height:1,close:vi.fn()} as unknown as ImageBitmap;
  const drawImage=vi.fn(),putImageData=vi.fn();
  vi.spyOn(HTMLCanvasElement.prototype,'getContext').mockReturnValue({drawImage,putImageData} as unknown as CanvasRenderingContext2D);
  vi.spyOn(f.controller.scene.visualPreview,'snapshot').mockReturnValue({phase:'ready',pixels:bitmap,time:0,generation:'1',error:null,runtime:null,cacheCompleted:0,cacheTotal:0,statistics:null,presentedFrames:1,discardedFrames:0});
  try {
    preferences({theme:'dark'});
    render(<WorkspaceProvider controller={f.controller}><Product/></WorkspaceProvider>);
    await waitFor(()=>expect(drawImage).toHaveBeenCalledWith(bitmap,0,0));
    const before=drawImage.mock.calls.filter(x=>x[0]===bitmap).length;
    act(()=>preferences({theme:'light'}));
    await waitFor(()=>expect(drawImage.mock.calls.filter(x=>x[0]===bitmap).length).toBeGreaterThan(before));
    expect(bitmap.close).not.toHaveBeenCalled();expect(putImageData).not.toHaveBeenCalled();
  } finally {cleanup();f.controller.dispose();preferences({theme:'dark'});}
});

it("draws each authorized bitmap immediately while canvas controls keep a stable React snapshot", async () => {
  const f=await setupFrame(), visual=f.controller.scene.visualPreview;
  const first={width:960,height:540,close:vi.fn()} as unknown as ImageBitmap;
  const second={width:960,height:540,close:vi.fn()} as unknown as ImageBitmap;
  const drawImage=vi.fn();
  vi.spyOn(HTMLCanvasElement.prototype,'getContext').mockReturnValue({drawImage} as unknown as CanvasRenderingContext2D);
  const view={phase:'ready' as const,pixels:first,time:0,generation:'1',error:null,runtime:null,
    cacheCompleted:1,cacheTotal:1,statistics:null,presentedFrames:1,discardedFrames:0};
  const snapshot=vi.spyOn(visual,'snapshot').mockReturnValue(view);
  const listeners=new Set<()=>void>();
  vi.spyOn(visual,'subscribe').mockImplementation(listener=>{listeners.add(listener);return()=>{listeners.delete(listener);};});
  try {
    render(<WorkspaceProvider controller={f.controller}><Product/></WorkspaceProvider>);
    const canvas=screen.getByRole('img',{name:'Live scene preview'});
    drawImage.mockClear();
    act(()=>{
      snapshot.mockReturnValue({...view,pixels:second,time:.04,presentedFrames:2});
      listeners.forEach(listener=>listener());
      expect(drawImage).toHaveBeenCalledWith(second,0,0);
      expect(screen.getByRole('region',{name:'Canvas notes'}).dataset.presentedFrames).toBe('2');
    });
    expect(screen.getByRole('img',{name:'Live scene preview'})).toBe(canvas);
    expect(canvas.getAttribute('data-time')).toBe('0.04');
    expect(second.close).not.toHaveBeenCalled();
  } finally {cleanup();f.controller.dispose();}
});

it("paints the replacement canvas after hiding pixels from another revision even when both snapshots have a bitmap",async()=>{
 const f=await setupFrame(),visual=f.controller.scene.visualPreview,scene=f.controller.getSnapshot().scene!;
 const first={width:960,height:540,close:vi.fn()} as unknown as ImageBitmap,second={width:1280,height:720,close:vi.fn()} as unknown as ImageBitmap;
 const drawImage=vi.fn();vi.spyOn(HTMLCanvasElement.prototype,"getContext").mockReturnValue({drawImage} as unknown as CanvasRenderingContext2D);
 const target=JSON.stringify([scene.head.project.id,scene.head.revision,scene.sequenceId]),view={phase:"ready" as const,pixels:first,target,time:0,generation:"1",error:null,runtime:null,cacheCompleted:0,cacheTotal:0,statistics:null,presentedFrames:1,discardedFrames:0};
 const snapshot=vi.spyOn(visual,"snapshot").mockReturnValue(view),listeners=new Set<()=>void>();vi.spyOn(visual,"subscribe").mockImplementation(listener=>{listeners.add(listener);return()=>{listeners.delete(listener);};});
 try{
  render(<WorkspaceProvider controller={f.controller}><Product/></WorkspaceProvider>);
  const old=screen.getByRole("img",{name:"Live scene preview"});
  act(()=>{snapshot.mockReturnValue({...view,target:"another revision"});listeners.forEach(listener=>listener());});
  expect(screen.queryByRole("img",{name:"Live scene preview"})).toBeNull();drawImage.mockClear();
  act(()=>{snapshot.mockReturnValue({...view,pixels:second});listeners.forEach(listener=>listener());});
  const current=screen.getByRole("img",{name:"Live scene preview"});expect(current).not.toBe(old);expect(current.getAttribute("width")).toBe("1280");expect(drawImage).toHaveBeenCalledWith(second,0,0);
 }finally{cleanup();f.controller.dispose();}
});

it("hands presentation to the worker without covering it with retained paused pixels and restores the same paused canvas",async()=>{
 const f=await setupFrame(),visual=f.controller.scene.visualPreview;
 const bitmap={width:640,height:360,close:vi.fn()} as unknown as ImageBitmap;
 vi.spyOn(HTMLCanvasElement.prototype,"getContext").mockReturnValue({drawImage:vi.fn()} as unknown as CanvasRenderingContext2D);
 vi.spyOn(visual,"setPlaybackCanvas").mockImplementation(()=>{});
 const view={phase:"ready" as const,pixels:bitmap,time:0,generation:"1",error:null,runtime:null,cacheCompleted:0,cacheTotal:0,statistics:null,presentedFrames:1,discardedFrames:0,surfaceGeneration:0,surfaceActive:false};
 const snapshot=vi.spyOn(visual,"snapshot").mockReturnValue(view),listeners=new Set<()=>void>();
 vi.spyOn(visual,"subscribe").mockImplementation(listener=>{listeners.add(listener);return()=>{listeners.delete(listener);};});
 try{
  f.controller.getSnapshot().scene!.audioPreview.phase="paused";
  render(<WorkspaceProvider controller={f.controller}><Product/></WorkspaceProvider>);
  const paused=screen.getByRole("img",{name:"Live scene preview"});
  act(()=>{snapshot.mockReturnValue({...view,surfaceGeneration:1,surfaceActive:true});listeners.forEach(listener=>listener());});
  expect(screen.getByRole("img",{name:"Live scene preview"}).getAttribute("data-preview-surface")).toBe("worker");
  expect(paused.style.display).toBe("none");expect(paused.getAttribute("aria-hidden")).toBe("true");
  expect(bitmap.close).not.toHaveBeenCalled();
  expect((screen.getByRole("button",{name:"Save snapshot"}) as HTMLButtonElement).disabled).toBe(false);
  expect((screen.getByRole("button",{name:"Copy snapshot"}) as HTMLButtonElement).disabled).toBe(false);
  f.controller.getSnapshot().scene!.audioPreview.phase="playing";
  act(()=>{snapshot.mockReturnValue({...view,surfaceGeneration:1,surfaceActive:false});listeners.forEach(listener=>listener());});
  expect(screen.getByRole("img",{name:"Live scene preview"})).toBe(paused);
  expect(paused.style.display).toBe("");expect(paused.getAttribute("aria-hidden")).toBeNull();
  expect((screen.getByRole("button",{name:"Save snapshot"}) as HTMLButtonElement).disabled).toBe(true);
 }finally{cleanup();f.controller.dispose();}
});
