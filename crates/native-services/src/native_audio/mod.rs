//! Native audio execution adapters. Application/composition retain authored
//! clocks, gain facts, policy and publication; this module owns only PCM/DSP.
mod channel;
pub mod dsp;
mod identity;
mod input;
mod preview;
mod preview_process;
mod preview_protocol;
mod preview_worker;
mod process;
mod protocol;
mod rolling;
mod rpc;
pub(crate) mod worker;
pub use preview_process::{
    NativeAudioPreviewPcmBlock, NativeAudioPreviewProcessConfig, NativeAudioPreviewProducer,
};
pub use process::{NativeAudioExecutionReceipt, NativeAudioProcess, NativeAudioProcessConfig};
mod pcm;
mod render;
mod source;
#[cfg(test)]
mod tests;
use crate::media::execution_error as media_error;
use opencut_contracts::EditorError;
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_AUDIO_PLAN", message)
}

/// Same application callbacks for local assembly and a worker byte transport.
/// This port consumes compiled facts; it never accepts a project document.
pub(crate) trait AudioSession<
    R: opencut_editor_application::classic_audio::ClassicAudioResources + ?Sized,
>
{
    fn plan(&self) -> Result<serde_json::Value, EditorError>;
    fn callback(
        &mut self,
        request: serde_json::Value,
        resources: &mut R,
    ) -> Result<serde_json::Value, EditorError>;
    fn compile_block(
        &mut self,
        request: opencut_editor_application::classic_audio::BlockRequest,
        resources: &mut R,
    ) -> Result<opencut_composition::scene::RuntimeAudioBlock, EditorError> {
        let value = self.callback(
            serde_json::json!({"op":"compileBlock","args":request}),
            resources,
        )?;
        serde_json::from_value(value).map_err(|_| invalid("Invalid native typed audio block"))
    }
    fn compile_preview(
        &mut self,
        request: opencut_editor_application::classic_audio::BindPreviewSources,
        resources: &mut R,
    ) -> Result<opencut_composition::scene::PreviewAudioBlock, EditorError> {
        let window = request
            .window
            .clone()
            .ok_or_else(|| invalid("Native preview binding requires its command window"))?;
        self.callback(
            serde_json::json!({"op":"bindPreviewSources","args":request}),
            resources,
        )?;
        let value = self.callback(
            serde_json::json!({"op":"compilePreviewBlock","args":window}),
            resources,
        )?;
        serde_json::from_value(value).map_err(|_| invalid("Invalid native typed preview block"))
    }
}
impl<R: opencut_editor_application::classic_audio::ClassicAudioResources + ?Sized> AudioSession<R>
    for opencut_editor_application::classic_audio::ClassicAudioSession<'_>
{
    fn plan(&self) -> Result<serde_json::Value, EditorError> {
        self.plan()
    }
    fn callback(
        &mut self,
        request: serde_json::Value,
        resources: &mut R,
    ) -> Result<serde_json::Value, EditorError> {
        self.callback(
            serde_json::from_value(request)
                .map_err(|_| invalid("Invalid typed native audio callback"))?,
            resources,
        )
    }
    fn compile_block(
        &mut self,
        request: opencut_editor_application::classic_audio::BlockRequest,
        resources: &mut R,
    ) -> Result<opencut_composition::scene::RuntimeAudioBlock, EditorError> {
        self.compile_export_block_typed(request, resources)
    }
    fn compile_preview(
        &mut self,
        request: opencut_editor_application::classic_audio::BindPreviewSources,
        resources: &mut R,
    ) -> Result<opencut_composition::scene::PreviewAudioBlock, EditorError> {
        let window = request
            .window
            .clone()
            .ok_or_else(|| invalid("Native preview binding requires its command window"))?;
        self.callback(
            opencut_editor_application::classic_audio::AudioCallback::BindPreview(request),
            resources,
        )?;
        self.compile_preview_block_typed(window, resources)
    }
}
fn callback<R, S>(
    session: &mut S,
    resources: &mut R,
    op: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, EditorError>
where
    R: opencut_editor_application::classic_audio::ClassicAudioResources + ?Sized,
    S: AudioSession<R>,
{
    resources.checkpoint()?;
    let result = session.callback(serde_json::json!({"op":op,"args":args}), resources)?;
    resources.checkpoint()?;
    Ok(result)
}
