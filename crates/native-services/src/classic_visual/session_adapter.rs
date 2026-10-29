//! Typed session adaptation only. Both frame versions use the same native
//! process lifetime, packet limits, cancellation and quota verification.
use opencut_contracts::EditorError;
use opencut_editor_application::media_frame_jobs::LocalVisualExecutionPolicy;
use opencut_editor_application::{classic_text_frame::*, classic_visual::*};
use serde::de::DeserializeOwned;
use serde_json::Value;

// One frame has at most 32 sources and 60 media layers. Batches change pipe
// scheduling only; every item still enters the original typed session.
pub(crate) const MAX_CALLBACK_BATCH: usize = 96;

fn batch_error(message: &str) -> EditorError {
    crate::native_process::Domain::Visual.protocol(crate::native_process::wire::error(message))
}

pub(crate) fn decode_callback_batch<T: DeserializeOwned>(
    value: &Value,
) -> Result<Vec<T>, EditorError> {
    if value
        .as_array()
        .is_none_or(|items| items.is_empty() || items.len() > MAX_CALLBACK_BATCH)
    {
        return Err(batch_error("Invalid native callback batch"));
    }
    serde_json::from_value(value.clone())
        .map_err(|_| batch_error("Native callback differs from typed session"))
}

pub(crate) trait VisualProcessSession<R: ClassicVisualResources + ?Sized> {
    type Callback: DeserializeOwned;
    type Receipt: DeserializeOwned;
    type Output;
    fn policy(&self) -> &LocalVisualExecutionPolicy;
    fn plan(&self) -> Result<Value, EditorError>;
    fn callback(
        &mut self,
        request: Self::Callback,
        resources: &mut R,
    ) -> Result<Value, EditorError>;
    fn callbacks(
        &mut self,
        requests: Vec<Self::Callback>,
        resources: &mut R,
        accept: &mut dyn FnMut(Value) -> Result<(), EditorError>,
    ) -> Result<(), EditorError> {
        if requests.is_empty() || requests.len() > MAX_CALLBACK_BATCH {
            return Err(batch_error("Invalid native callback batch"));
        }
        for request in requests {
            resources.checkpoint()?;
            let value = self.callback(request, resources)?;
            resources.checkpoint()?;
            accept(value)?;
        }
        Ok(())
    }
    fn rgba_chunk(
        &mut self,
        offset: u64,
        bytes: &[u8],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<(), EditorError>;
    fn finish(
        &self,
        receipt: Self::Receipt,
        quota: VisualQuotaReceipt,
        runtime: ClassicVisualRuntimeReceipt,
    ) -> Result<Self::Output, EditorError>;
}
impl<R: ClassicVisualResources + ?Sized> VisualProcessSession<R> for ClassicVisualSession<'_> {
    type Callback = VisualCallback;
    type Receipt = ClassicVisualRenderReceipt;
    type Output = ClassicVisualExecutionReceipt;
    fn policy(&self) -> &LocalVisualExecutionPolicy {
        ClassicVisualSession::policy(self)
    }
    fn plan(&self) -> Result<Value, EditorError> {
        ClassicVisualSession::plan(self)
    }
    fn callback(
        &mut self,
        request: Self::Callback,
        resources: &mut R,
    ) -> Result<Value, EditorError> {
        ClassicVisualSession::callback(self, request, resources)
    }
    fn rgba_chunk(
        &mut self,
        offset: u64,
        bytes: &[u8],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<(), EditorError> {
        ClassicVisualSession::rgba_chunk(self, offset, bytes, sink)
    }
    fn finish(
        &self,
        receipt: Self::Receipt,
        quota: VisualQuotaReceipt,
        runtime_identity: ClassicVisualRuntimeReceipt,
    ) -> Result<Self::Output, EditorError> {
        self.validate_receipt(&receipt)?;
        Ok(ClassicVisualExecutionReceipt {
            receipt,
            quota,
            runtime_identity,
        })
    }
}
impl<R: ClassicTextFrameResources + ?Sized> VisualProcessSession<R>
    for ClassicTextFrameSession<'_>
{
    type Callback = TextFrameCallback;
    type Receipt = ClassicTextFrameReceipt;
    type Output = ClassicTextFrameExecution;
    fn policy(&self) -> &LocalVisualExecutionPolicy {
        ClassicTextFrameSession::policy(self)
    }
    fn plan(&self) -> Result<Value, EditorError> {
        ClassicTextFrameSession::plan(self)
    }
    fn callback(
        &mut self,
        request: Self::Callback,
        resources: &mut R,
    ) -> Result<Value, EditorError> {
        ClassicTextFrameSession::callback(self, request, resources)
    }
    fn rgba_chunk(
        &mut self,
        offset: u64,
        bytes: &[u8],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<(), EditorError> {
        ClassicTextFrameSession::rgba_chunk(self, offset, bytes, sink)
    }
    fn finish(
        &self,
        receipt: Self::Receipt,
        quota: VisualQuotaReceipt,
        runtime_identity: ClassicVisualRuntimeReceipt,
    ) -> Result<Self::Output, EditorError> {
        self.validate_receipt(&receipt)?;
        Ok(ClassicTextFrameExecution {
            receipt,
            quota,
            runtime_identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Resources {
        revoked: bool,
        checks: usize,
    }
    impl ClassicVisualResources for Resources {
        fn checkpoint(&mut self) -> Result<(), EditorError> {
            self.checks += 1;
            if self.revoked {
                Err(EditorError::new(
                    "NOT_AUTHORIZED",
                    "Revoked between batch items",
                ))
            } else {
                Ok(())
            }
        }
        fn resolve(&mut self, _: &VisualResource, _: &str) -> Result<(String, u64), EditorError> {
            unreachable!()
        }
        fn read(&mut self, _: &str, _: u64, _: u32) -> Result<(Vec<u8>, bool), EditorError> {
            unreachable!()
        }
        fn release(&mut self, _: &str) -> Result<(), EditorError> {
            unreachable!()
        }
    }
    struct Session {
        policy: LocalVisualExecutionPolicy,
        calls: Vec<u8>,
        revoke: Option<u8>,
    }
    impl Session {
        fn new(revoke: Option<u8>) -> Self {
            Self {
                policy: LocalVisualExecutionPolicy {
                    max_frame_pixels: 1,
                    max_decoded_pixels: 1,
                    max_gpu_bytes: 1024,
                    max_rgba_bytes: 1024,
                    max_encoded_bytes: 1024,
                    max_output_bytes: 1024,
                    max_rss_bytes: 1024,
                    deadline_ms: 1000,
                },
                calls: Vec::new(),
                revoke,
            }
        }
    }
    impl VisualProcessSession<Resources> for Session {
        type Callback = u8;
        type Receipt = Value;
        type Output = ();
        fn policy(&self) -> &LocalVisualExecutionPolicy {
            &self.policy
        }
        fn plan(&self) -> Result<Value, EditorError> {
            unreachable!()
        }
        fn callback(
            &mut self,
            request: u8,
            resources: &mut Resources,
        ) -> Result<Value, EditorError> {
            if usize::from(request) != self.calls.len() + 1 {
                return Err(EditorError::new(
                    "INVALID_VISUAL_PLAN",
                    "Repeated or discontinuous callback",
                ));
            }
            self.calls.push(request);
            if self.revoke == Some(request) {
                resources.revoked = true;
            }
            Ok(Value::from(request))
        }
        fn rgba_chunk(
            &mut self,
            _: u64,
            _: &[u8],
            _: &mut dyn ClassicVisualRgbaSink,
        ) -> Result<(), EditorError> {
            unreachable!()
        }
        fn finish(
            &self,
            _: Value,
            _: VisualQuotaReceipt,
            _: ClassicVisualRuntimeReceipt,
        ) -> Result<(), EditorError> {
            unreachable!()
        }
    }
    #[test]
    fn callback_batches_preserve_order_and_each_authority_fence() {
        let mut resources = Resources::default();
        let mut session = Session::new(None);
        let mut replies = Vec::new();
        session
            .callbacks(vec![1, 2, 3], &mut resources, &mut |v| {
                replies.push(v);
                Ok(())
            })
            .unwrap();
        assert_eq!(session.calls, [1, 2, 3]);
        assert_eq!(replies, [Value::from(1), Value::from(2), Value::from(3)]);
        assert_eq!(resources.checks, 6);
        let mut resources = Resources::default();
        let mut session = Session::new(Some(2));
        let mut replies = Vec::new();
        let error = session
            .callbacks(vec![1, 2, 3], &mut resources, &mut |v| {
                replies.push(v);
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.code, "NOT_AUTHORIZED");
        assert_eq!(session.calls, [1, 2]);
        assert_eq!(
            replies,
            [Value::from(1)],
            "A revoked item's reply cannot leave the parent"
        );
        let mut resources = Resources::default();
        let mut session = Session::new(None);
        let mut replies = Vec::new();
        let error = session
            .callbacks(vec![1, 1, 2], &mut resources, &mut |v| {
                replies.push(v);
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.code, "INVALID_VISUAL_PLAN");
        assert_eq!(session.calls, [1]);
        assert_eq!(replies, [Value::from(1)]);
    }
    #[test]
    fn invalid_batch_envelopes_are_rejected_before_any_callback() {
        for value in [
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!([1, "invalid", 3]),
            serde_json::json!(vec![1; MAX_CALLBACK_BATCH + 1]),
        ] {
            assert!(decode_callback_batch::<u8>(&value).is_err());
        }
        assert_eq!(
            decode_callback_batch::<u8>(&serde_json::json!([1, 2, 3])).unwrap(),
            [1, 2, 3]
        );
        let mut resources = Resources::default();
        let mut session = Session::new(None);
        let error = session
            .callbacks(vec![1, 2, 3], &mut resources, &mut |_| {
                Err(EditorError::new("JOB_CANCELLED", "Output cancelled"))
            })
            .unwrap_err();
        assert_eq!(error.code, "JOB_CANCELLED");
        assert_eq!(
            session.calls,
            [1],
            "A failed output cannot advance later bindings"
        );
    }
}
