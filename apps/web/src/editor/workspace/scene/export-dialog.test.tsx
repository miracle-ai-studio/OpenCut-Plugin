// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { WorkspaceProvider } from "../context";
import { ExportDialog } from "./export-dialog";
import { setupAudioWav } from "./audio-wav-fixture";
import type { Job } from "../../../lib/editor-sdk";
beforeAll(()=>{HTMLDialogElement.prototype.showModal=function(){this.setAttribute("open","");};HTMLDialogElement.prototype.close=function(){this.removeAttribute("open");};});
beforeEach(()=>sessionStorage.clear());
afterEach(()=>{cleanup();vi.restoreAllMocks();});
it("retires the dialog on disconnect without reading capabilities from a closed project",async()=>{
 const f=await setupAudioWav();try{
  render(<WorkspaceProvider controller={f.controller}><ExportDialog onClose={vi.fn()} showRecords={vi.fn()}/></WorkspaceProvider>);
  await waitFor(()=>expect((screen.getByRole("button",{name:"Start video export"}) as HTMLButtonElement).disabled).toBe(false));
  await act(async()=>{f.controller.disconnect();});expect(screen.queryByRole("dialog")).toBeNull();
 }finally{f.controller.dispose();}
});
it("switches MP4/WebM/WAV in a single dialog without editing the project or submitting a job",async()=>{
 const f=await setupAudioWav();try{
  const before=structuredClone(f.current()),onClose=vi.fn();render(<WorkspaceProvider controller={f.controller}><ExportDialog onClose={onClose} showRecords={vi.fn()}/></WorkspaceProvider>);
  await waitFor(()=>expect((screen.getByRole("button",{name:"Start video export"}) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect((screen.getByLabelText("Resolution") as HTMLSelectElement).value).toBe("p1080");
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"webm"}});expect((screen.getByLabelText("Format") as HTMLSelectElement).value).toBe("webm");
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"wav"}});expect(screen.getByLabelText("Sample rate")).toBeTruthy();expect(screen.queryByRole("checkbox",{name:"Include audio"})).toBeNull();
  fireEvent.change(screen.getByLabelText("Export scope"),{target:{value:"range"}});expect(screen.getByLabelText("Start time (seconds)")).toBeTruthy();
  expect(f.current()).toEqual(before);expect(f.requests).toHaveLength(0);expect(f.rangeRequests).toHaveLength(0);
  fireEvent.click(screen.getAllByRole("button",{name:"Close"}).at(-1)!);expect(onClose).toHaveBeenCalledOnce();
 }finally{f.controller.dispose();}
});
it("keeps video and WAV drafts across format switches without exposing hidden controls to focus",async()=>{
 const f=await setupAudioWav();try{
  render(<WorkspaceProvider controller={f.controller}><ExportDialog onClose={vi.fn()} showRecords={vi.fn()}/></WorkspaceProvider>);
  await waitFor(()=>expect((screen.getByLabelText("Format") as HTMLSelectElement).disabled).toBe(false));
  fireEvent.change(screen.getByLabelText("Resolution"),{target:{value:"p2160"}});
  fireEvent.change(screen.getByLabelText("Quality"),{target:{value:"medium"}});fireEvent.click(screen.getByRole("checkbox",{name:"Include audio"}));
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"wav"}});fireEvent.change(screen.getByLabelText("Export scope"),{target:{value:"range"}});
  fireEvent.change(screen.getByLabelText("Start time (seconds)"),{target:{value:"2"}});fireEvent.change(screen.getByLabelText("End time (seconds)"),{target:{value:"10"}});fireEvent.change(screen.getByLabelText("Sample rate"),{target:{value:"44100"}});
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"mp4"}});expect((screen.getByLabelText("Resolution") as HTMLSelectElement).value).toBe("p2160");expect((screen.getByLabelText("Quality") as HTMLSelectElement).value).toBe("medium");expect((screen.getByRole("checkbox",{name:"Include audio"}) as HTMLInputElement).checked).toBe(false);
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"wav"}});expect((screen.getByLabelText("Export scope") as HTMLSelectElement).value).toBe("range");expect((screen.getByLabelText("Start time (seconds)") as HTMLInputElement).value).toBe("2");expect((screen.getByLabelText("End time (seconds)") as HTMLInputElement).value).toBe("10");expect((screen.getByLabelText("Sample rate") as HTMLSelectElement).value).toBe("44100");
  expect(screen.queryByRole("button",{name:"Start video export"})).toBeNull();expect(f.requests).toHaveLength(0);expect(f.rangeRequests).toHaveLength(0);
 }finally{f.controller.dispose();}
});
it.each(["p720", "p1080", "p1440", "p2160"])("submits selected WebM resolution %s through the original fixed revision exporter exactly once",async(resolution)=>{
 const f=await setupAudioWav();try{
  const previous=f.call.getMockImplementation()!,requests:unknown[]=[];
  f.call.mockImplementation(async(name,input,options)=>{
   if(name==="opencut_render_submit" && (input as {kind:string}).kind==="video"){
    const r=(input as {request:{project_id:string;revision:string;sequence_id:string;idempotency_key:string}}).request;requests.push(structuredClone(input));
    return {job_id:"export-one",kind:"render.video",state:"queued",cancel_requested:false,target:{project_id:r.project_id,revision:r.revision,sequence_id:r.sequence_id},artifacts:[],result:null} as Job as never;
   }return previous(name,input,options);
  });
  render(<WorkspaceProvider controller={f.controller}><ExportDialog onClose={vi.fn()} showRecords={vi.fn()}/></WorkspaceProvider>);
  await waitFor(()=>expect((screen.getByRole("button",{name:"Start video export"}) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.change(screen.getByLabelText("Resolution"),{target:{value:resolution}});
  fireEvent.change(screen.getByLabelText("Format"),{target:{value:"webm"}});fireEvent.click(screen.getByRole("button",{name:"Start video export"}));
  await screen.findByRole("button",{name:"View export records"});expect(requests).toHaveLength(1);expect(requests[0]).toMatchObject({kind:"video",request:{revision:"r0",options:{format:"webm",resolution}}});
  expect((screen.getByLabelText("Format") as HTMLSelectElement).disabled).toBe(true);expect(f.current().revision).toBe("r0");
 }finally{f.controller.dispose();}
});
