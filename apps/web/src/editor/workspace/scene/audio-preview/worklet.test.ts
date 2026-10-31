import { afterEach, expect, it, vi } from "vitest";
afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });
it("starts after six blocks while retaining bounded jitter storage; short EOF and stopped cursor cannot deadlock or advance silence", async () => {
  class Processor {
    port = { onmessage: null as null | ((event: { data: Record<string, unknown> }) => void), postMessage: vi.fn() };
    process(_inputs: Float32Array[][], _outputs: Float32Array[][]) { return true; }
  }
  let Current = Processor;
  vi.stubGlobal("AudioWorkletProcessor", Processor);
  vi.stubGlobal("currentFrame", 128);
  vi.stubGlobal("registerProcessor", (_name: string, ctor: typeof Processor) => { Current = ctor; });
  await import("./worklet");
  const p = new Current();
  const send = (data: Record<string, unknown>) => p.port.onmessage!({ data });
  const left = new Float32Array(2), right = new Float32Array(2);
  let frame=128;
  const quantum = () => {vi.stubGlobal("currentFrame",frame);frame+=left.length;return p.process([], [[left, right]]);};
  send({ kind: "prime", blocks: 6 });
  send({ kind: "reset", token: "first" });
  send({ kind: "block", token: "first", pcm: new Float32Array([.1, .2, .3, .4]).buffer });
  quantum(); expect([...left]).toEqual([0, 0]); expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({phase: "preparing", consumedFrames: "0"}));
  send({ kind: "block", token: "first", pcm: new Float32Array([.5, .6, .7, .8]).buffer });
  for (let i = 0; i < 3; i++) send({ kind: "block", token: "first", pcm: new Float32Array([.5, .6, .7, .8]).buffer });
  quantum(); expect([...left]).toEqual([0, 0]);
  send({ kind: "block", token: "first", pcm: new Float32Array([.5, .6, .7, .8]).buffer });
  quantum(); expect(left[0]).toBeCloseTo(.1);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "2", copied: 2, renderFrame: 132, quantumFrames: 2 }));
  send({ kind: "stop" });
  expect(p.port.postMessage).toHaveBeenLastCalledWith({ kind: "stopped", token: "first", consumedFrames: "2" });
  send({ kind: "reset", token: "short" });
  send({ kind: "block", token: "short", pcm: new Float32Array([.25, .75]).buffer });
  send({ kind: "end", token: "first" }); quantum(); expect([...left]).toEqual([0, 0]);
  send({ kind: "end", token: "short" }); quantum(); expect([...left]).toEqual([.25, 0]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "1", copied: 1, phase: "ended" }));
  frame=256;
  quantum();
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "1", copied: 0, renderFrame: 256, quantumFrames: 2, underflowEvents:0,underflowFrames:"0" }));
  send({ kind: "stop" });
  send({ kind: "prime", blocks: 2 });
  send({ kind: "reset", token: "small-host" });
  send({ kind: "block", token: "small-host", pcm: new Float32Array([.1, .2, .3, .4]).buffer });
  quantum(); expect([...left]).toEqual([0, 0]);
  send({ kind: "block", token: "small-host", pcm: new Float32Array([.5, .6, .7, .8]).buffer });
  quantum(); expect(left[0]).toBeCloseTo(.1);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "2", copied: 2 }));
  quantum(); quantum();
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "4", copied: 0 }));
  send({ kind: "block", token: "small-host", pcm: new Float32Array([.25, .75, .5, 1]).buffer });
  quantum(); expect([...left]).toEqual([0, 0]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "4", copied: 0 }));
  send({ kind: "block", token: "small-host", pcm: new Float32Array([.6, .6, .7, .7]).buffer });
  quantum(); expect([...left]).toEqual([.25, .5]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "6", copied: 2,underflowEvents:1,underflowFrames:"4" }));

});

it("stalls at picture credit without consuming PCM and resumes the exact samples across port/reset ordering", async () => {
  class Processor {
    port = { onmessage: null as null | ((event: { data: Record<string, unknown> }) => void), postMessage: vi.fn() };
    process(_inputs: Float32Array[][], _outputs: Float32Array[][]) { return true; }
  }
  let Current = Processor;
  vi.stubGlobal("AudioWorkletProcessor", Processor);
  vi.stubGlobal("currentFrame", 128);
  vi.stubGlobal("registerProcessor", (_name: string, ctor: typeof Processor) => { Current = ctor; });
  await import("./worklet");
  const p = new Current(), gate = { onmessage: null as null | ((event: { data: unknown }) => void), start: vi.fn() };
  const send = (data: Record<string, unknown>) => p.port.onmessage!({ data });
  const limit = (token: string, frames: string) => gate.onmessage!({ data: { kind: "picture-limit", token, frames } });
  const left = new Float32Array(2), right = new Float32Array(2);
  let frame=128;
  const quantum = () => {vi.stubGlobal("currentFrame",frame);frame+=left.length;return p.process([], [[left, right]]);};
  send({ kind: "picture-port", token: "first", port: gate });
  limit("first", "1"); // Separate control port may arrive before data-port reset.
  send({ kind: "reset", token: "first" });
  send({ kind: "block", token: "first", pcm: new Float32Array([.1,.2,.3,.4]).buffer });
  send({ kind: "end", token: "first" });
  quantum(); expect(left[0]).toBeCloseTo(.1); expect(left[1]).toBe(0);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "1", copied: 1, phase: "buffering", elapsedRenderFrames:"1" }));
  limit("old", "100"); quantum(); expect([...left]).toEqual([0,0]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "1", copied: 0, completedBlocks: 0 }));
  limit("first", "2"); quantum(); expect(left[0]).toBeCloseTo(.3); expect(right[0]).toBeCloseTo(.4);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ consumedFrames: "2", copied: 1, completedBlocks: 1, elapsedRenderFrames:"5" }));
  send({ kind: "stop" });
  expect(p.port.postMessage).toHaveBeenLastCalledWith({ kind: "stopped", token: "first", consumedFrames: "2" });
});

it("reports the identical sample interval directly to the clock without giving it device submission ownership",async()=>{
  class Processor {
    port={onmessage:null as null|((event:{data:Record<string,unknown>})=>void),postMessage:vi.fn()};
    process(_inputs:Float32Array[][],_outputs:Float32Array[][]){return true;}
  }
  let Current=Processor;
  vi.stubGlobal('AudioWorkletProcessor',Processor);vi.stubGlobal('currentFrame',128);
  vi.stubGlobal('registerProcessor',(_name:string,ctor:typeof Processor)=>{Current=ctor;});
  await import('./worklet');const p=new Current(),clock={postMessage:vi.fn()},viewer={postMessage:vi.fn(),start:vi.fn()};
  const send=(data:Record<string,unknown>)=>p.port.onmessage!({data});
  send({kind:'clock-port',token:'first',port:clock});send({kind:'reset',token:'first'});
  send({kind:'presentation-clock-port',token:'first',port:viewer});
  send({kind:'block',token:'first',pcm:new Float32Array([.25,.75]).buffer});send({kind:'end',token:'first'});
  p.process([],[[new Float32Array(2),new Float32Array(2)]]);
  expect(clock.postMessage.mock.calls[0][0]).toEqual(p.port.postMessage.mock.calls.at(-1)![0]);
  expect(viewer.postMessage.mock.calls[0][0]).toEqual(clock.postMessage.mock.calls[0][0]);
  expect(clock.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({consumedFrames:'1',copied:1,runStartFrame:128,runStartConsumed:'0'}));
  send({kind:'stop'});expect(clock.postMessage).toHaveBeenCalledOnce();
  expect(viewer.postMessage).toHaveBeenLastCalledWith({kind:'stopped',token:'first',consumedFrames:'1'});
  send({kind:'reset',token:'other'});p.process([],[[new Float32Array(2),new Float32Array(2)]]);
  expect(clock.postMessage).toHaveBeenCalledOnce();
  expect(viewer.postMessage).toHaveBeenCalledTimes(2);
});


it("serves a repeated device quantum identically without skipping PCM or crossing a reset",async()=>{
  class Processor {port={onmessage:null as null|((e:{data:Record<string,unknown>})=>void),postMessage:vi.fn()};process(_i:Float32Array[][],_o:Float32Array[][]){return true;}}
  let Current=Processor;vi.stubGlobal("AudioWorkletProcessor",Processor);vi.stubGlobal("currentFrame",128);
  vi.stubGlobal("registerProcessor",(_name:string,ctor:typeof Processor)=>{Current=ctor;});await import("./worklet");
  const p=new Current(),send=(data:Record<string,unknown>)=>p.port.onmessage!({data});
  send({kind:"reset",token:"first"});send({kind:"block",token:"first",pcm:new Float32Array([.25,.5,.75,1]).buffer});send({kind:"end",token:"first"});
  const render=()=>{const channels=[new Float32Array(1),new Float32Array(1)];p.process([],[channels]);return channels.map(c=>c[0]);};
  expect(render()).toEqual([.25,.5]);const count=p.port.postMessage.mock.calls.length;
  expect(render()).toEqual([.25,.5]);expect(p.port.postMessage).toHaveBeenCalledTimes(count);
  vi.stubGlobal("currentFrame",129);expect(render()).toEqual([.75,1]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({consumedFrames:"2",repeatedQuanta:1}));
  send({kind:"reset",token:"next"});send({kind:"block",token:"next",pcm:new Float32Array([.125,.375]).buffer});send({kind:"end",token:"next"});
  expect(render()).toEqual([0,0]);vi.stubGlobal("currentFrame",130);expect(render()).toEqual([.125,.375]);
  expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({token:"next",consumedFrames:"1"}));
});

it('accepts contiguous picture credit on the presentation port and ignores delayed lower or foreign credit',async()=>{
 class Processor{port={onmessage:null as null|((e:{data:Record<string,unknown>})=>void),postMessage:vi.fn()};process(_i:Float32Array[][],_o:Float32Array[][]){return true;}}
 let Current=Processor;vi.stubGlobal('AudioWorkletProcessor',Processor);vi.stubGlobal('currentFrame',128);vi.stubGlobal('registerProcessor',(_name:string,ctor:typeof Processor)=>{Current=ctor;});
 await import('./worklet');const p=new Current(),main={onmessage:null as null|((e:{data:unknown})=>void),start:vi.fn()},viewer={onmessage:null as null|((e:{data:unknown})=>void),start:vi.fn(),postMessage:vi.fn()};
 const send=(data:Record<string,unknown>)=>p.port.onmessage!({data}),credit=(port:typeof main,token:string,frames:string)=>port.onmessage!({data:{kind:'picture-limit',token,frames}});
 send({kind:'picture-port',token:'first',port:main});credit(main,'first','0');send({kind:'reset',token:'first'});send({kind:'presentation-clock-port',token:'first',port:viewer});
 send({kind:'block',token:'first',pcm:new Float32Array([.1,.2,.3,.4,.5,.6,.7,.8]).buffer});send({kind:'end',token:'first'});
 credit(viewer,'first','2');credit(main,'first','0');const left=new Float32Array(2),right=new Float32Array(2);p.process([],[[left,right]]);expect(left[1]).toBeCloseTo(.3);
 credit(viewer,'old','100');credit(viewer,'first','NaN');vi.stubGlobal('currentFrame',130);p.process([],[[left,right]]);expect([...left]).toEqual([0,0]);
 credit(viewer,'first','4');credit(main,'first','3');vi.stubGlobal('currentFrame',132);p.process([],[[left,right]]);expect(left[0]).toBeCloseTo(.5);expect(left[1]).toBeCloseTo(.7);
 expect(p.port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({consumedFrames:'4',copied:2}));
});

it('keeps every PCM sample in order while native pictures catch up under the bounded presentation lead',async()=>{
 const {pictureConsumptionLimit}=await import('./presentation-clock');
 class Processor{port={onmessage:null as null|((e:{data:Record<string,unknown>})=>void),postMessage:vi.fn()};process(_i:Float32Array[][],_o:Float32Array[][]){return true;}}
 let Current=Processor;vi.stubGlobal('AudioWorkletProcessor',Processor);vi.stubGlobal('currentFrame',0);vi.stubGlobal('registerProcessor',(_name:string,c:typeof Processor)=>{Current=c;});await import('./worklet');
 const p=new Current(),gate={onmessage:null as null|((e:{data:unknown})=>void),start:vi.fn()};
 const send=(data:Record<string,unknown>)=>p.port.onmessage!({data});
 const rate=48000,total=rate*3,step=1600;
 const session={identity:{sample_rate:rate},actual_start_sample:'0',total_frames:String(total)} as import('../types').AudioPreviewSession;
 const credit=(through:number)=>gate.onmessage!({data:{kind:'picture-limit',token:'run',frames:String(pictureConsumptionLimit({ticks:String(through),timebase:rate},session))}});
 send({kind:'prime',blocks:2});send({kind:'picture-port',token:'run',port:gate});credit(0);send({kind:'reset',token:'run'});
 let sent=0,issued=0,completed=0,copied=0,reported=0,through=step*5,prepared=through,readyAt=1728,wall=0,ended=false,error=0,rendered=5;
 const value=(n:number)=>Math.fround((n%127)/127-.5);
 const left=new Float32Array(128),right=new Float32Array(128);
 credit(through);
 while(copied<total&&wall<total+rate){
  while(sent-completed<32&&issued<total){const count=Math.min(4096,total-issued),pcm=new Float32Array(count*2);for(let i=0;i<count;i++){pcm[i*2]=value(issued+i);pcm[i*2+1]=-value(issued+i);}issued+=count;sent++;send({kind:'block',token:'run',pcm:pcm.buffer});}
  if(issued===total&&!ended){send({kind:'end',token:'run'});ended=true;}
  if(wall>=readyAt){through=prepared+step;credit(through);rendered++;prepared=Math.max(through,reported);readyAt=wall+1728;}
  vi.stubGlobal('currentFrame',wall);p.process([],[[left,right]]);
  const receipt=p.port.postMessage.mock.calls.at(-1)![0];completed=receipt.completedBlocks;
  for(let i=0;i<128;i++){const expected=i<receipt.copied?value(copied+i):0;error=Math.max(error,Math.abs(left[i]-expected),Math.abs(right[i]+expected));}
  copied=Number(receipt.consumedFrames);wall+=128;if(wall%2400<128)reported=copied;
 }
 const final=p.port.postMessage.mock.calls.at(-1)![0];
 expect(copied).toBe(total);expect(error).toBe(0);expect(rendered).toBeLessThan(90);
 // The final sample stays fenced until its native end picture completes.
 // There is no accumulating picture-induced silence across the scene.
 expect(Number(final.elapsedRenderFrames)-copied).toBeLessThan(2400);
 expect(final.underflowEvents).toBe(0);send({kind:'stop'});
 expect(p.port.postMessage).toHaveBeenLastCalledWith({kind:'stopped',token:'run',consumedFrames:String(total)});
});

it.each([44100, 48000])("starts before filling the transport queue and retains exact PCM through a 500 ms producer gap at %i Hz", async rate => {
  class Processor {
    port = { onmessage: null as null | ((e: { data: Record<string, unknown> }) => void), postMessage: vi.fn() };
    process(_inputs: Float32Array[][], _outputs: Float32Array[][]) { return true; }
  }
  let Current = Processor;
  vi.stubGlobal("AudioWorkletProcessor", Processor);
  vi.stubGlobal("registerProcessor", (_name: string, ctor: typeof Processor) => { Current = ctor; });
  await import("./worklet");
  const p = new Current(), send = (data: Record<string, unknown>) => p.port.onmessage!({ data });
  let issued = 0, copied = 0, wall = 0;
  const value = (n: number) => Math.fround((n % 251) / 251 - .5);
  const block = () => {
    const pcm = new Float32Array(8192);
    for (let i = 0; i < 4096; i++) { pcm[i * 2] = value(issued + i); pcm[i * 2 + 1] = -value(issued + i); }
    issued += 4096;
    send({ kind: "block", token: "run", pcm: pcm.buffer });
  };
  const render = () => {
    const left = new Float32Array(128), right = new Float32Array(128);
    vi.stubGlobal("currentFrame", wall); wall += 128;
    p.process([], [[left, right]]);
    const receipt = p.port.postMessage.mock.calls.at(-1)![0];
    for (let i = 0; i < 128; i++) {
      const expected = i < receipt.copied ? value(copied + i) : 0;
      expect(left[i]).toBe(expected); expect(right[i]).toBe(expected === 0 ? 0 : -expected);
    }
    copied = Number(receipt.consumedFrames);
    return receipt;
  };
  send({ kind: "reset", token: "run" });
  for (let i = 0; i < 7; i++) block();
  expect(render().consumedFrames).toBe("0");
  block();
  expect(render().copied).toBe(128);
  // An observed producer pause must consume existing samples without a gap.
  for (let i = 0; i < Math.ceil(rate * .5 / 128); i++) expect(render().copied).toBe(128);
  // Partial front blocks still occupy a slot: refill all 32 slots, not just 8.
  const retainedBlocks = 8 - Math.floor(copied / 4096);
  for (let i = retainedBlocks; i < 32; i++) block();
  send({ kind: "end", token: "run" });
  while (copied < issued) expect(render().copied).toBe(128);
  const tail = render();
  expect(tail.phase).toBe("ended"); expect(tail.underflowEvents).toBe(0); expect(tail.underflowFrames).toBe("0");
  expect(copied).toBe(issued);
  send({ kind: "stop" });
  expect(p.port.postMessage).toHaveBeenLastCalledWith({ kind: "stopped", token: "run", consumedFrames: String(issued) });
});
