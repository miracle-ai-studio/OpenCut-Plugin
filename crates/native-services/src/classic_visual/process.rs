use super::session_adapter::VisualProcessSession;
use crate::native_visual::NativeProcess;
use opencut_contracts::EditorError;
use opencut_editor_application::{
    classic_text_frame::*, classic_visual::*, media_frame_jobs::LocalVisualExecutionPolicy,
};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct ClassicVisualProcessConfig {
    pub worker: PathBuf,
    pub deadline: Duration,
    pub policy: LocalVisualExecutionPolicy,
    pub strict_cgroup: Option<PathBuf>,
}
pub struct ClassicVisualProcess {
    pub config: ClassicVisualProcessConfig,
}
fn error(code: &str, message: &str) -> EditorError {
    EditorError::new(code, message)
}
impl ClassicVisualProcess {
    #[cfg(unix)]
    pub(crate) fn render_session<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: Value,
        fonts: Option<Value>,
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<S::Output, EditorError> {
        let handles: Vec<ClassicVisualResourceHandle> = serde_json::from_value(handles)
            .map_err(|_| error("INVALID_VISUAL_PLAN", "Invalid native source handles"))?;
        let fonts: Vec<ClassicTextFontHandle> =
            serde_json::from_value(fonts.unwrap_or_else(|| serde_json::json!([])))
                .map_err(|_| error("INVALID_VISUAL_PLAN", "Invalid native font handles"))?;
        let mut process =
            NativeProcess::start(self.config.clone(), self.config.deadline, None, &mut || {
                resources.checkpoint()
            })?;
        let receipt = process.render(session, resources, begin, &handles, &fonts, sink)?;
        let (runtime, quota) = process.finish(&mut || resources.checkpoint())?;
        validate_quota(&quota, &self.config, Some(self.config.deadline))?;
        session.finish(receipt, quota, runtime)
    }
}
pub(crate) fn validate_quota(
    quota: &VisualQuotaReceipt,
    config: &ClassicVisualProcessConfig,
    elapsed: Option<Duration>,
) -> Result<(), EditorError> {
    let strict = config.strict_cgroup.is_some();
    if quota.profile
        != if strict {
            "strict-hard"
        } else {
            "monitored-terminate"
        }
        || quota.max_rss_bytes != config.policy.max_rss_bytes.to_string()
        || quota
            .observed_peak_rss_bytes
            .parse::<u64>()
            .ok()
            .is_none_or(|n| n > config.policy.max_rss_bytes)
        || quota.hard_memory_enforced != strict
        || quota.monitor_interval_ms != 25
        || elapsed.is_some_and(|budget| u128::from(quota.elapsed_ms) > budget.as_millis())
    {
        return Err(error(
            "VISUAL_PROTOCOL_ERROR",
            "Native quota receipt differs from captured admission",
        ));
    }
    Ok(())
}
impl ClassicVisualExecutor for ClassicVisualProcess {
    fn execute(
        &self,
        session: &mut ClassicVisualSession<'_>,
        resources: &mut dyn ClassicVisualResources,
        handles: &[ClassicVisualResourceHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicVisualExecutionReceipt, EditorError> {
        #[cfg(unix)]
        {
            self.render_session(
                session,
                resources,
                "begin",
                serde_json::to_value(handles)
                    .map_err(|_| error("INVALID_VISUAL_PLAN", "Invalid source handles"))?,
                None,
                sink,
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (session, resources, handles, sink);
            Err(error(
                "CAPABILITY_UNAVAILABLE",
                "Native process lifecycle unavailable",
            ))
        }
    }
}
impl ClassicTextFrameExecutor for ClassicVisualProcess {
    fn execute_text_frame(
        &self,
        session: &mut ClassicTextFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicTextFrameExecution, EditorError> {
        #[cfg(unix)]
        {
            self.render_session(
                session,
                resources,
                "beginTextFrame",
                serde_json::to_value(handles)
                    .map_err(|_| error("INVALID_VISUAL_PLAN", "Invalid source handles"))?,
                Some(
                    serde_json::to_value(fonts)
                        .map_err(|_| error("INVALID_VISUAL_PLAN", "Invalid font handles"))?,
                ),
                sink,
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (session, resources, handles, fonts, sink);
            Err(error(
                "CAPABILITY_UNAVAILABLE",
                "Native process lifecycle unavailable",
            ))
        }
    }
}
