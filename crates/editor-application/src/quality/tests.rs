use super::*;
use crate::{
    JobRecord, JobState,
    queries::{EventAnchor, JobPageSlice},
    source_art::SourceArtPolicy,
};
use opencut_job_execution::scene_audio::TrustedJobAuthority;
use std::cell::Cell;

struct Fixture {
    job: JobRecord,
    source: AuthorizedSourceArt,
    source_reads: Cell<usize>,
    native_calls: Cell<usize>,
    revoked: bool,
}
impl JobRead for Fixture {
    fn read_job(&self, id: &str) -> Result<JobRecord, EditorError> {
        assert_eq!(id, self.job.job_id);
        Ok(self.job.clone())
    }
    fn read_job_for_cursor(&self, _: &str) -> Result<(JobRecord, String), EditorError> {
        unreachable!()
    }
    fn read_jobs(&self) -> Result<Vec<JobRecord>, EditorError> {
        unreachable!()
    }
    fn read_job_page(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: usize,
    ) -> Result<JobPageSlice, EditorError> {
        unreachable!()
    }
    fn read_job_changes(
        &self,
        _: Option<&EventAnchor>,
        _: Option<&str>,
        _: usize,
    ) -> Result<Vec<crate::JobEvent>, EditorError> {
        unreachable!()
    }
    fn read_job_events(
        &self,
        _: i64,
        _: usize,
        _: Option<&str>,
    ) -> Result<Vec<crate::JobEvent>, EditorError> {
        unreachable!()
    }
}
impl SourceArtRead for Fixture {
    fn fixed_source_art_snapshot(
        &self,
        project: &str,
        revision: &str,
    ) -> Result<AuthorizedSourceArt, EditorError> {
        self.source_reads.set(self.source_reads.get() + 1);
        assert_eq!(project, self.source.snapshot.project.id);
        assert_eq!(revision, self.source.snapshot.revision);
        if self.revoked {
            return Err(EditorError::new("NOT_AUTHORIZED", "Revoked fixed source"));
        }
        Ok(self.source.clone())
    }
    fn revalidate_source_art(
        &self,
        _: &str,
        _: &str,
        _: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        unreachable!()
    }
}
impl QualityNative for Fixture {
    fn measure(&self, input: &PreparedQuality) -> Result<MeasuredQuality, EditorError> {
        self.native_calls.set(self.native_calls.get() + 1);
        assert_eq!(input.job_id, self.job.job_id);
        assert_eq!(input.attempt, self.job.attempt);
        assert_eq!(input.artifact.content_hash, "d".repeat(64));
        assert_eq!(input.source.snapshot, self.source.snapshot);
        Err(EditorError::new(
            "TEST_NATIVE_REACHED",
            "Observed authorized prepared quality input",
        ))
    }
}
fn fixture(range: bool) -> (Fixture, QualityInspect) {
    let (mut prepared, mut stored) = crate::export_video::tests::stored_export_fixture();
    if range {
        prepared.range = Some(export::VideoFrameRange {
            start_frame: "0".into(),
            frame_count: "60".into(),
        });
        let plan = export::prepare(&prepared).unwrap();
        stored.public.identity = export::public_identity(&prepared, &plan);
        stored.public.render_input_sha256 = export::render_input_sha256(&prepared).unwrap();
    }
    let job_id = format!("job_{}", "a".repeat(32));
    let input = QualityInspect {
        artifact_id: format!("artifact:{job_id}:{}", "d".repeat(64)),
        caption_frame_job_ids: vec![],
    };
    let source = AuthorizedSourceArt {
        snapshot: prepared.execution.snapshot.clone(),
        authority: TrustedJobAuthority::LocalOwner,
        scope: "quality-regression".into(),
        policy: SourceArtPolicy {
            max_encoded_bytes: 1024,
            max_pcm_bytes: 1024,
            max_rss_bytes: 4096,
            deadline_ms: 1000,
        },
    };
    let job = JobRecord {
        job_id,
        state: JobState::Succeeded,
        attempt: 2,
        kind: if range {
            export::EXPORT_VIDEO_RANGE_KIND
        } else {
            export::EXPORT_VIDEO_KIND
        }
        .into(),
        input: serde_json::to_value(prepared).unwrap(),
        result: Some(serde_json::to_value(stored).unwrap()),
        error: None,
        lease_owner: None,
        lease_until: None,
        cancel_requested: false,
        progress: None,
    };
    (
        Fixture {
            job,
            source,
            source_reads: Cell::new(0),
            native_calls: Cell::new(0),
            revoked: false,
        },
        input,
    )
}
#[test]
fn projected_whole_and_range_exports_reach_native_with_the_original_authorized_artifact() {
    for range in [false, true] {
        let (store, input) = fixture(range);
        let view = crate::job_views::project(store.job.clone());
        assert_eq!(
            view.kind,
            if range {
                "render.video_range"
            } else {
                "render.video"
            }
        );
        assert!(view.result_unavailable.is_none());
        let error = inspect(&store, &store, input).unwrap_err();
        assert_eq!(error.code, "TEST_NATIVE_REACHED");
        assert_eq!(store.source_reads.get(), 1);
        assert_eq!(store.native_calls.get(), 1);
    }
}
#[test]
fn unsupported_unpublished_and_foreign_artifacts_never_reach_native_analysis() {
    for scenario in ["unsupported", "unfinished", "foreign"] {
        let (mut store, mut input) = fixture(false);
        match scenario {
            "unsupported" => store.job.kind = "render.project_scene_video.v4".into(),
            "unfinished" => store.job.state = JobState::Running,
            "foreign" => {
                input.artifact_id = format!("artifact:{}:{}", store.job.job_id, "f".repeat(64))
            }
            _ => unreachable!(),
        }
        assert_eq!(
            inspect(&store, &store, input).unwrap_err().code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(store.source_reads.get(), 0);
        assert_eq!(store.native_calls.get(), 0);
    }
}
#[test]
fn revoked_source_is_rejected_after_export_validation_before_native_analysis() {
    let (mut store, input) = fixture(false);
    store.revoked = true;
    assert_eq!(
        inspect(&store, &store, input).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
    assert_eq!(store.source_reads.get(), 1);
    assert_eq!(store.native_calls.get(), 0);
}
