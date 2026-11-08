import {useEffect,useId,useLayoutEffect,useRef,useState} from "react";
import {useWorkspace} from "../context";
import {Button,Field} from "../../shared/ui";
import {t} from "../../shared/locale";
import {decimalSecondsHelp,formatTime,secondsForDisplay,timeFromDecimalSeconds} from "../../model/time";
import {ChevronLeft,ChevronRight,Crosshair} from "lucide-react";
import {addTime,subtractTime,compareTime,zero} from "../../model/time";
import {sequenceContentEnd} from "./sequence-navigation";
import {sequenceEnd} from "./timeline-edit/geometry";
import {AudioPreviewControls} from "./audio-preview-controls";

/** Only the transport display subscribes to the continuous output cursor. */
export function PlaybackFooter({toolsRef}:{toolsRef?:(node:HTMLDivElement|null)=>void}={}) {
  const {controller,scene}=useWorkspace();
  const [seek,setSeek]=useState("0");
  const [invalidSeek,setInvalidSeek]=useState(false);
  const seekHelpId=useId();
  const intent=useRef({dirty:false,focused:false});
  const input=useRef<HTMLInputElement>(null),time=useRef<HTMLSpanElement>(null),seekTime=useRef<HTMLSpanElement>(null);
  useLayoutEffect(()=>{
    let lastAt=-Infinity;
    const draw=()=>{
      if(!controller.getSnapshot().scene)return;
      const now=performance.now();if(now-lastAt<100)return;lastAt=now;
      const at=controller.scene.currentPlayhead(),label=formatTime(at);
      if(time.current)time.current.textContent=label;
      if(seekTime.current)seekTime.current.textContent=label;
      if(input.current&&!intent.current.focused&&!intent.current.dirty)input.current.value=secondsForDisplay(at).toFixed(6);
    };
    draw();return controller.scene.audioPreview.subscribePresentation(draw);
  },[controller,scene?.playhead,scene?.audioPreview.phase]);
  useEffect(()=>{
    intent.current={dirty:false,focused:false};
    setInvalidSeek(false);
    if(scene)setSeek(secondsForDisplay(scene.playhead).toFixed(6));
  },[scene?.head.project.id,scene?.head.revision,scene?.historical?.revision,scene?.sequenceId]);
  useEffect(()=>{
    if(scene&&!intent.current.dirty&&!intent.current.focused)setSeek(secondsForDisplay(scene.playhead).toFixed(6));
  },[scene?.playhead]);
  if(!scene)return null;
  const sequence=(scene.historical??scene.head).project.sequences.find(s=>s.id===scene.sequenceId)!;
  const cancel=()=>{
    intent.current={dirty:false,focused:false};
    setInvalidSeek(false);
    setSeek(secondsForDisplay(controller.scene.currentPlayhead()).toFixed(6));
  };
  const submit=()=>{
    try {
      controller.scene.seek(timeFromDecimalSeconds(input.current?.value??seek));
      intent.current={dirty:false,focused:false};
      setInvalidSeek(false);
      const current=controller.getSnapshot().scene;
      if(current)setSeek(secondsForDisplay(current.playhead).toFixed(6));
    } catch(error){
      if(error instanceof Error && error.message===decimalSecondsHelp)setInvalidSeek(true);
      else controller.report(error);
    }
  };
  const step=async(direction:number)=>{
    try{
      if(["playing","loading","stalled"].includes(scene.audioPreview.phase))await controller.scene.audioPreview.pause();
      const snapshot=scene.historical??scene.head;if(snapshot.project.settings.profile!=="classic_global")return;
      const fps=snapshot.project.settings.settings.fps,one={ticks:String(fps.denominator),timebase:fps.numerator};
      const current=controller.scene.currentPlayhead(),next=direction>0?addTime(current,one):subtractTime(current,one),end=sequenceContentEnd(sequence);
      controller.scene.seek(compareTime(next,zero)<0?zero:compareTime(next,end)>0?end:next);
    }catch(error){controller.report(error);}
  };
  return <div className="workbench-player-footer">
    <div className="workbench-time-display"><span ref={time}>{formatTime(scene.playhead)}</span><span>/</span><span>{formatTime({ticks:String(Math.round(sequenceEnd(sequence)*120000)),timebase:120000})}</span>
      <details className="workbench-seek-popover" onKeyDown={event=>event.stopPropagation()}><summary aria-label={t("Go to time")} title={t("Go to time")}><Crosshair size={13}/></summary>
      <div className="workbench-seek-control">
        <Field label={t("Playhead (seconds)")}><input ref={input} inputMode="decimal" value={seek}
          aria-describedby={seekHelpId} aria-invalid={invalidSeek || undefined}
          onFocus={()=>{intent.current.focused=true;}}
          onBlur={()=>{intent.current.focused=false;if(!intent.current.dirty)setSeek(secondsForDisplay(controller.scene.currentPlayhead()).toFixed(6));}}
          onChange={event=>{intent.current.dirty=true;setSeek(event.target.value);setInvalidSeek(false);}}
          onKeyDown={event=>{if(event.key==="Enter"){event.preventDefault();submit();}if(event.key==="Escape"){event.preventDefault();cancel();event.currentTarget.closest("details")?.removeAttribute("open");}}}/></Field>
        <Button onClick={submit}>{t("Go to time")}</Button><span ref={seekTime}>{formatTime(scene.playhead)}</span>
        <p id={seekHelpId} className="workbench-seek-help" role={invalidSeek ? "alert" : undefined}>{t(decimalSecondsHelp)}</p>
      </div></details>
    </div>
    <div className="workbench-transport-group"><Button aria-label={t("Previous frame")} title={t("Previous frame")} icon={<ChevronLeft size={17}/>} onClick={()=>void step(-1)}/><AudioPreviewControls compact/><Button aria-label={t("Next frame")} title={t("Next frame")} icon={<ChevronRight size={17}/>} onClick={()=>void step(1)}/></div>
    <div className="workbench-player-tools" ref={toolsRef}/>
  </div>;
}
