//! Native text coverage is established from authorized CAS descriptors on each
//! execution. No browser claim, source URL or system font can establish it.
use crate::browser_resources::{VerifiedBrowserResources, VerifiedMediaFiles};
#[cfg(test)]
use crate::font_import::StaticFontInspector;
use opencut_composition::scene::{
    NativeTextCoverage, PreparedCaptionFrame, PreparedSceneFrame, PreparedTextFrame,
    PreparedVisualFrame,
};
use opencut_contracts::{EditorError, scene::FontResource, text_frame::TextFontResource};
use opencut_editor_application::{
    classic_text_frame::{ClassicTextFontHandle, ClassicTextFrameResources},
    classic_visual::{ClassicVisualResourceHandle, ClassicVisualResources, VisualResource},
    font_catalog::{validate_catalog_font_envelope, validate_catalog_font_policy},
    font_import::{FontImportPolicy, ParsedFontFace},
    media_video_jobs::PreparedVideoFrame,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const INVALID: &str = "INVALID_VISUAL_PLAN";
fn invalid(message: &str) -> EditorError {
    EditorError::new(INVALID, message)
}
fn font_resource(font: &FontResource) -> TextFontResource {
    TextFontResource {
        font_id: font.id.clone(),
        content_hash: font.content.content_hash.clone(),
        size: font.content.size.to_string(),
        weight: font.weight,
        style: font.style,
        face_index: font.face_index,
    }
}
// ECMAScript whitespace used by Classic's /\s+/ greedy word split. Do not
// inspect tabs/separators as glyphs when V3 never renders those codepoints.
fn caption_separator(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
}
struct FontText<'a> {
    font: &'a FontResource,
    content: String,
}
fn font_texts<'a>(
    prepared: &'a PreparedTextFrame,
    policy: &FontImportPolicy,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    caption_ids: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<&'a str, FontText<'a>>, EditorError> {
    selected_font_texts(prepared, policy, checkpoint, caption_ids, None)
}
fn selected_font_texts<'a>(
    prepared: &'a PreparedTextFrame,
    policy: &FontImportPolicy,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    caption_ids: Option<&BTreeSet<String>>,
    selected: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<&'a str, FontText<'a>>, EditorError> {
    // Validate policy even for a frame with no text. Aggregate UTF-8 work, not
    // just a single title, is bounded before opening any CAS descriptor.
    validate_catalog_font_policy(policy)?;
    if selected.is_some_and(|ids| {
        ids.iter().any(|id| {
            !prepared
                .texts()
                .iter()
                .any(|text| &text.identity.element_id == id)
        })
    }) {
        return Err(invalid(
            "Font proof selection contains an unknown text element",
        ));
    }
    let mut total = 0usize;
    let mut fonts: BTreeMap<&str, FontText<'_>> = BTreeMap::new();
    let mut selected_fonts = BTreeSet::new();
    for text in prepared.texts() {
        checkpoint()?;
        if text.font.face_index != 0 {
            return Err(EditorError::new(
                "UNSUPPORTED_FONT",
                "Text frames require a represented static face at index zero",
            ));
        }
        let is_caption = caption_ids.is_some_and(|ids| ids.contains(&text.identity.element_id));
        if !is_caption {
            // Preserve V2/free-text validation and error ordering exactly.
            validate_catalog_font_envelope(
                &text.font.content.content_hash,
                text.font.content.size,
                &text.text.content,
                policy,
            )?;
        }
        total = total.checked_add(text.text.content.len()).ok_or_else(|| {
            EditorError::new(
                "FONT_BUDGET_EXCEEDED",
                "Text frame coverage budget overflow",
            )
        })?;
        if total > policy.max_text_bytes {
            return Err(EditorError::new(
                "FONT_BUDGET_EXCEEDED",
                "Complete frame text exceeds the inspection budget",
            ));
        }
        let normalized;
        let required = if is_caption {
            normalized = text
                .text
                .content
                .chars()
                .map(|c| if caption_separator(c) { ' ' } else { c })
                .collect::<String>();
            normalized.as_str()
        } else {
            text.text.content.as_str()
        };
        if is_caption {
            validate_catalog_font_envelope(
                &text.font.content.content_hash,
                text.font.content.size,
                required,
                policy,
            )?;
        }
        let batch = fonts.entry(&text.font.id).or_insert_with(|| FontText {
            font: &text.font,
            content: String::new(),
        });
        if batch.font != &text.font {
            return Err(invalid("One font id has different fixed resources"));
        }
        // The complete frame still passes the original envelope and resource
        // consistency checks. Only the selected text contributes native glyph
        // work; cached text sharing this font must not be inspected again.
        if selected.is_none_or(|ids| ids.contains(&text.identity.element_id)) {
            selected_fonts.insert(text.font.id.as_str());
            batch.content.push_str(required);
        }
    }
    if selected.is_some() {
        fonts.retain(|id, _| selected_fonts.contains(id));
    }
    Ok(fonts)
}

/// One video attempt owns a bounded set of verified media descriptors and one
/// native font inspection. It retains bounded immutable font bytes, never grants.
/// Font hash/size, text, wrapping proof or policy changes invalidate inspection.
#[derive(Default)]
pub struct VideoResourceCache {
    inspection: Option<FontInspection>,
    descriptors: VerifiedMediaFiles,
    profile_rows: Vec<serde_json::Value>,
    profile_path: Option<std::path::PathBuf>,
}
struct FontInspection {
    hash: String,
    size: usize,
    text: String,
    required_space: bool,
    max_font_bytes: u64,
    max_text_bytes: usize,
    parsed: ParsedFontFace,
}
fn inspect_font_with_space(
    bytes: &[u8],
    hash: &str,
    text: &str,
    required_space: bool,
    policy: &FontImportPolicy,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
) -> Result<ParsedFontFace, EditorError> {
    let parsed =
        crate::font_inspection::inspect_verified_resource(bytes, hash, text, policy, checkpoint)?;
    if required_space {
        crate::font_inspection::inspect_verified_resource(bytes, hash, " ", policy, checkpoint)?;
    }
    Ok(parsed)
}
impl VideoResourceCache {
    fn inspect_verified(
        &mut self,
        bytes: &[u8],
        hash: &str,
        text: &str,
        required_space: bool,
        policy: &FontImportPolicy,
        checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    ) -> Result<ParsedFontFace, EditorError> {
        checkpoint()?;
        if let Some(prior) = &self.inspection {
            if prior.hash == hash
                && prior.size == bytes.len()
                && prior.text == text
                && prior.required_space == required_space
                && prior.max_font_bytes == policy.max_font_bytes
                && prior.max_text_bytes == policy.max_text_bytes
            {
                return Ok(prior.parsed.clone());
            }
        }
        let parsed =
            inspect_font_with_space(bytes, hash, text, required_space, policy, checkpoint)?;
        checkpoint()?;
        // No growing cache across captions or frames. The current envelope has
        // already bounded this text, and the entry is dropped with the attempt.
        self.inspection = Some(FontInspection {
            hash: hash.into(),
            size: bytes.len(),
            text: text.into(),
            required_space,
            max_font_bytes: policy.max_font_bytes,
            max_text_bytes: policy.max_text_bytes,
            parsed: parsed.clone(),
        });
        Ok(parsed)
    }
}

pub struct VerifiedClassicTextResources<F> {
    inner: VerifiedBrowserResources<F>,
    fonts: BTreeMap<String, TextFontResource>,
    coverage: Vec<NativeTextCoverage>,
}
impl<F: FnMut() -> Result<(), EditorError>> VerifiedClassicTextResources<F> {
    /// Job-local verified media descriptors remain fixed across frames. Fonts
    /// keep complete immutable byte/native proofs; every bounded read checks current policy.
    pub fn new_video_frame(
        root: &Path,
        prepared: &PreparedVideoFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
        cache: &mut VideoResourceCache,
    ) -> Result<Self, EditorError> {
        match prepared {
            PreparedVideoFrame::Text(frame) => Self::new_inner(
                root,
                frame,
                font_policy,
                checkpoint,
                true,
                None,
                None,
                None,
                Some(cache),
            ),
            PreparedVideoFrame::Caption(frame) => Self::new_caption_inner(
                root,
                frame,
                font_policy,
                checkpoint,
                true,
                None,
                Some(cache),
            ),
            PreparedVideoFrame::Graphic(frame) => Self::new_caption_inner(
                root,
                frame.execution(),
                font_policy,
                checkpoint,
                true,
                None,
                Some(cache),
            ),
            PreparedVideoFrame::Scene(frame) => Self::new_scene_inner(
                root,
                frame,
                font_policy,
                checkpoint,
                true,
                None,
                Some(cache),
            ),
            PreparedVideoFrame::Mask(frame) => Self::new_caption_inner(
                root,
                frame.execution().execution(),
                font_policy,
                checkpoint,
                true,
                None,
                Some(cache),
            ),
            PreparedVideoFrame::Blend(frame) => Self::new_caption_inner(
                root,
                frame.execution().execution(),
                font_policy,
                checkpoint,
                true,
                None,
                Some(cache),
            ),
        }
    }

    pub fn new(
        root: &Path,
        prepared: &PreparedTextFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::new_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            true,
            None,
            None,
            None,
            None,
        )
    }
    /// Caption wrapping can normalize tabs and Unicode separators to U+0020.
    /// Inspect that inserted codepoint from the same authorized bytes while
    /// retaining original text identities in the native coverage proofs.
    pub fn new_caption_frame(
        root: &Path,
        prepared: &PreparedCaptionFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::new_caption_inner(root, prepared, font_policy, checkpoint, true, None, None)
    }
    /// Original caption-font proof without opening any media for live preview.
    pub fn new_caption_fonts(
        root: &Path,
        prepared: &PreparedCaptionFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::new_caption_inner(root, prepared, font_policy, checkpoint, false, None, None)
    }
    /// Native-only incremental preview proof. The host selects missing current
    /// elements after matching its own session-scoped proofs; selectors and
    /// proofs are never accepted from a public request. An empty selection still
    /// validates the whole frame envelope and cancellation, without opening CAS.
    pub fn new_caption_fonts_subset(
        root: &Path,
        prepared: &PreparedCaptionFrame,
        selected: &BTreeSet<String>,
        font_policy: &FontImportPolicy,
        mut checkpoint: F,
    ) -> Result<Vec<NativeTextCoverage>, EditorError> {
        if selected.is_empty() {
            checkpoint()?;
            let caption_ids = prepared
                .captions()
                .iter()
                .map(|caption| caption.element_id.clone())
                .collect();
            selected_font_texts(
                prepared.execution(),
                font_policy,
                &mut checkpoint,
                Some(&caption_ids),
                Some(selected),
            )?;
            checkpoint()?;
            return Ok(Vec::new());
        }
        let resources = Self::new_caption_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            false,
            Some(selected),
            None,
        )?;
        Ok(resources.coverage)
    }
    fn new_caption_inner(
        root: &Path,
        prepared: &PreparedCaptionFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
        verify_media: bool,
        selected: Option<&BTreeSet<String>>,
        cache: Option<&mut VideoResourceCache>,
    ) -> Result<Self, EditorError> {
        let caption_ids: BTreeSet<String> = prepared
            .captions()
            .iter()
            .map(|c| c.element_id.clone())
            .collect();
        let space_fonts: BTreeSet<_> = prepared
            .execution()
            .texts()
            .iter()
            .filter(|text| caption_ids.contains(text.identity.element_id.as_str()))
            .map(|text| text.font.id.clone())
            .collect();
        Self::new_inner(
            root,
            prepared.execution(),
            font_policy,
            checkpoint,
            verify_media,
            Some(&space_fonts),
            Some(&caption_ids),
            selected,
            cache,
        )
    }
    /// Font proof for browser preview without opening or hashing media again.
    pub fn new_fonts(
        root: &Path,
        prepared: &PreparedTextFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::new_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            false,
            None,
            None,
            None,
            None,
        )
    }
    /// Draft subtitle elements are ordinary TextV4, but their observed greedy
    /// wrapping normalizes whitespace and can introduce an ASCII space. Use
    /// the existing caption coverage rules without weakening free-text V2.
    pub fn new_subtitle_authoring_fonts(
        root: &Path,
        prepared: &PreparedTextFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        let caption_ids = prepared
            .texts()
            .iter()
            .map(|text| text.identity.element_id.clone())
            .collect::<BTreeSet<_>>();
        let space_fonts = prepared
            .texts()
            .iter()
            .map(|text| text.font.id.clone())
            .collect::<BTreeSet<_>>();
        Self::new_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            false,
            Some(&space_fonts),
            Some(&caption_ids),
            None,
            None,
        )
    }
    pub fn new_scene_frame(
        root: &Path,
        prepared: &PreparedSceneFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::new_scene_inner(root, prepared, font_policy, checkpoint, true, None, None)
    }
    pub fn new_scene_frame_cached(
        root: &Path,
        prepared: &PreparedSceneFrame,
        font_policy: &FontImportPolicy,
        checkpoint: F,
        cache: &mut VideoResourceCache,
    ) -> Result<Self, EditorError> {
        Self::new_scene_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            true,
            None,
            Some(cache),
        )
    }
    pub fn new_scene_fonts_subset(
        root: &Path,
        prepared: &PreparedSceneFrame,
        selected: &BTreeSet<String>,
        font_policy: &FontImportPolicy,
        checkpoint: F,
    ) -> Result<Vec<NativeTextCoverage>, EditorError> {
        let mut checkpoint = checkpoint;
        if selected.is_empty() {
            checkpoint()?;
            let captions = prepared.execution().execution();
            let text = captions.execution();
            let caption_ids = captions
                .captions()
                .iter()
                .map(|p| p.element_id.clone())
                .collect();
            selected_font_texts(
                text,
                font_policy,
                &mut checkpoint,
                Some(&caption_ids),
                Some(selected),
            )?;
            let mut total = text.texts().iter().try_fold(0usize, |n, p| {
                n.checked_add(p.text.content.len())
                    .ok_or_else(|| invalid("Scene text envelope overflow"))
            })?;
            for mask in prepared.mask_texts() {
                validate_catalog_font_envelope(
                    &mask.font.content.content_hash,
                    mask.font.content.size,
                    &mask.mask.content,
                    font_policy,
                )?;
                total = total
                    .checked_add(mask.mask.content.len())
                    .ok_or_else(|| invalid("Scene text envelope overflow"))?;
                if total > font_policy.max_text_bytes {
                    return Err(EditorError::new(
                        "FONT_BUDGET_EXCEEDED",
                        "Complete scene text exceeds the inspection budget",
                    ));
                }
                checkpoint()?;
            }
            checkpoint()?;
            return Ok(vec![]);
        }
        Ok(Self::new_scene_inner(
            root,
            prepared,
            font_policy,
            checkpoint,
            false,
            Some(selected),
            None,
        )?
        .coverage)
    }
    fn new_scene_inner(
        root: &Path,
        prepared: &PreparedSceneFrame,
        font_policy: &FontImportPolicy,
        mut checkpoint: F,
        include_media: bool,
        selected: Option<&BTreeSet<String>>,
        cache: Option<&mut VideoResourceCache>,
    ) -> Result<Self, EditorError> {
        checkpoint()?;
        let captions = prepared.execution().execution();
        let text = captions.execution();
        let title_ids: BTreeSet<_> = text
            .texts()
            .iter()
            .map(|p| p.identity.element_id.clone())
            .collect();
        let mask_ids: BTreeSet<_> = prepared
            .mask_texts()
            .iter()
            .map(|p| p.mask_id.clone())
            .collect();
        if mask_ids.len() != prepared.mask_texts().len()
            || mask_ids.iter().any(|id| title_ids.contains(id))
            || selected.is_some_and(|ids| {
                ids.iter()
                    .any(|id| !title_ids.contains(id) && !mask_ids.contains(id))
            })
        {
            return Err(invalid(
                "Scene font proof contains an unknown or repeated text owner",
            ));
        }
        let title_selected = selected.map(|ids| {
            ids.intersection(&title_ids)
                .cloned()
                .collect::<BTreeSet<_>>()
        });
        let caption_ids: BTreeSet<_> = captions
            .captions()
            .iter()
            .map(|p| p.element_id.clone())
            .collect();
        let space_fonts: BTreeSet<_> = text
            .texts()
            .iter()
            .filter(|p| caption_ids.contains(&p.identity.element_id))
            .map(|p| p.font.id.clone())
            .collect();
        let mut batches = selected_font_texts(
            text,
            font_policy,
            &mut checkpoint,
            Some(&caption_ids),
            title_selected.as_ref(),
        )?;
        let mut total = text.texts().iter().try_fold(0usize, |total, p| {
            total
                .checked_add(p.text.content.len())
                .ok_or_else(|| invalid("Scene text envelope overflow"))
        })?;
        for mask in prepared.mask_texts() {
            checkpoint()?;
            validate_catalog_font_envelope(
                &mask.font.content.content_hash,
                mask.font.content.size,
                &mask.mask.content,
                font_policy,
            )?;
            total = total
                .checked_add(mask.mask.content.len())
                .ok_or_else(|| invalid("Scene text envelope overflow"))?;
            if total > font_policy.max_text_bytes {
                return Err(EditorError::new(
                    "FONT_BUDGET_EXCEEDED",
                    "Complete scene text exceeds the inspection budget",
                ));
            }
            if mask.font.face_index != 0 {
                return Err(EditorError::new(
                    "UNSUPPORTED_FONT",
                    "Text masks require a represented static face at index zero",
                ));
            }
            if selected.is_none_or(|ids| ids.contains(&mask.mask_id)) {
                let batch = batches
                    .entry(mask.font.id.as_str())
                    .or_insert_with(|| FontText {
                        font: &mask.font,
                        content: String::new(),
                    });
                if batch.font != &mask.font {
                    return Err(invalid("Scene text font identity differs between owners"));
                }
                batch.content.push_str(&mask.mask.content);
            }
        }
        let coverage = text
            .texts()
            .iter()
            .filter(|p| selected.is_none_or(|ids| ids.contains(&p.identity.element_id)))
            .map(|p| NativeTextCoverage {
                element_id: p.identity.element_id.clone(),
                font: p.font.clone(),
                content: p.text.content.clone(),
            })
            .chain(
                prepared
                    .mask_texts()
                    .iter()
                    .filter(|p| selected.is_none_or(|ids| ids.contains(&p.mask_id)))
                    .map(|p| NativeTextCoverage {
                        element_id: p.mask_id.clone(),
                        font: p.font.clone(),
                        content: p.mask.content.clone(),
                    }),
            )
            .collect();
        Self::new_verified(
            root,
            text.media(),
            batches,
            font_policy,
            checkpoint,
            include_media,
            Some(&space_fonts),
            coverage,
            cache,
        )
    }
    fn new_inner(
        root: &Path,
        prepared: &PreparedTextFrame,
        font_policy: &FontImportPolicy,
        mut checkpoint: F,
        include_media: bool,
        space_fonts: Option<&BTreeSet<String>>,
        caption_ids: Option<&BTreeSet<String>>,
        selected: Option<&BTreeSet<String>>,
        cache: Option<&mut VideoResourceCache>,
    ) -> Result<Self, EditorError> {
        checkpoint()?;
        let batches = match selected {
            Some(_) => selected_font_texts(
                prepared,
                font_policy,
                &mut checkpoint,
                caption_ids,
                selected,
            )?,
            None => font_texts(prepared, font_policy, &mut checkpoint, caption_ids)?,
        };
        let coverage = prepared
            .texts()
            .iter()
            .filter(|text| selected.is_none_or(|ids| ids.contains(&text.identity.element_id)))
            .map(|text| NativeTextCoverage {
                element_id: text.identity.element_id.clone(),
                font: text.font.clone(),
                content: text.text.content.clone(),
            })
            .collect();
        Self::new_verified(
            root,
            prepared.media(),
            batches,
            font_policy,
            checkpoint,
            include_media,
            space_fonts,
            coverage,
            cache,
        )
    }
    fn new_verified<'a>(
        root: &Path,
        media: &PreparedVisualFrame,
        batches: BTreeMap<&'a str, FontText<'a>>,
        font_policy: &FontImportPolicy,
        checkpoint: F,
        include_media: bool,
        space_fonts: Option<&BTreeSet<String>>,
        coverage: Vec<NativeTextCoverage>,
        mut cache: Option<&mut VideoResourceCache>,
    ) -> Result<Self, EditorError> {
        let started = std::time::Instant::now();
        let mut transfer_ms = 0.0;
        let mut inspection_ms = 0.0;
        let versions = media
            .resources()
            .iter()
            .filter(|_| include_media)
            .map(|r| (format!("media:{}", r.asset_id), &r.version))
            .chain(
                batches
                    .iter()
                    .map(|(id, batch)| (format!("font:{id}"), &batch.font.content)),
            );
        let mut inner = if let Some(cache) = cache.as_deref_mut() {
            VerifiedBrowserResources::new_versions_cached(
                root,
                versions,
                checkpoint,
                INVALID,
                &mut cache.descriptors,
            )?
        } else {
            VerifiedBrowserResources::new_versions(root, versions, checkpoint, INVALID)?
        };
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        let handles: BTreeMap<String, String> = inner
            .handles()
            .map(|(id, handle)| (id.into(), handle.into()))
            .collect();
        for (id, batch) in &batches {
            let transferred = std::time::Instant::now();
            let key = format!("font:{id}");
            let handle = handles
                .get(&key)
                .ok_or_else(|| invalid("Prepared font descriptor is missing"))?;
            let version = &batch.font.content;
            let bytes = if let Some(bytes) = inner.immutable_font(&key)? {
                bytes
            } else {
                let (transfer, size) =
                    inner.resolve(&key, &version.content_hash, version.size, handle)?;
                let mut bytes = Vec::with_capacity(size as usize);
                let mut digest = Sha256::new();
                let mut offset = 0u64;
                while offset < size {
                    // read checks the live authority/cancellation checkpoint on
                    // both sides of every bounded descriptor read.
                    let (chunk, eof) = inner.read(&transfer, offset, 65536)?;
                    if chunk.is_empty() || chunk.len() as u64 > size - offset {
                        return Err(invalid("Incomplete font descriptor transfer"));
                    }
                    offset += chunk.len() as u64;
                    digest.update(&chunk);
                    bytes.extend_from_slice(&chunk);
                    if eof != (offset == size) {
                        return Err(invalid("Font transfer EOF differs from its fixed size"));
                    }
                }
                inner.release(&transfer)?;
                if format!("{:x}", digest.finalize()) != version.content_hash {
                    return Err(EditorError::new(
                        "ASSET_INTEGRITY_ERROR",
                        "Font descriptor bytes changed before native inspection",
                    ));
                }
                let bytes: std::sync::Arc<[u8]> = bytes.into();
                if let Some(cache) = cache.as_deref_mut() {
                    inner.retain_font(&key, version, bytes.clone(), &mut cache.descriptors)?;
                }
                bytes
            };
            let required_space = space_fonts.is_some_and(|fonts| fonts.contains(*id))
                && !batch.content.contains(' ');
            transfer_ms += transferred.elapsed().as_secs_f64() * 1000.0;
            let inspected = std::time::Instant::now();
            let parsed = match cache.as_deref_mut() {
                Some(cache) => cache.inspect_verified(
                    &bytes,
                    &version.content_hash,
                    &batch.content,
                    required_space,
                    font_policy,
                    &mut || inner.checkpoint(),
                )?,
                None => inspect_font_with_space(
                    &bytes,
                    &version.content_hash,
                    &batch.content,
                    required_space,
                    font_policy,
                    &mut || inner.checkpoint(),
                )?,
            };
            inspection_ms += inspected.elapsed().as_secs_f64() * 1000.0;
            if parsed.family != batch.font.family
                || parsed.weight != batch.font.weight
                || parsed.style != batch.font.style
            {
                return Err(EditorError::new(
                    "FONT_FACE_MISMATCH",
                    "Fixed font family, weight or style differs from its actual face",
                ));
            }
            inner.checkpoint()?;
        }
        if let (Some(cache), Some(directory)) = (
            cache.as_deref_mut(),
            std::env::var_os("OPENCUT_VIDEO_PROFILE_DIR"),
        ) {
            if cache.profile_rows.len() < 50 {
                let path = cache
                    .profile_path
                    .get_or_insert_with(|| {
                        std::path::PathBuf::from(directory)
                            .join(format!("resources-{}.json", uuid::Uuid::new_v4()))
                    })
                    .clone();
                cache.profile_rows.push(serde_json::json!({"open_ms":open_ms,"transfer_ms":transfer_ms,"inspection_ms":inspection_ms,"total_ms":started.elapsed().as_secs_f64()*1000.0}));
                if let Ok(bytes) = serde_json::to_vec(&cache.profile_rows) {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
        let fonts = batches
            .values()
            .map(|batch| (batch.font.id.clone(), font_resource(batch.font)))
            .collect();
        Ok(Self {
            inner,
            fonts,
            coverage,
        })
    }
    pub fn coverage(&self) -> &[NativeTextCoverage] {
        &self.coverage
    }
    pub fn handles(&self) -> Vec<ClassicVisualResourceHandle> {
        self.inner
            .handles()
            .filter_map(|(id, handle)| {
                id.strip_prefix("media:")
                    .map(|asset_id| ClassicVisualResourceHandle {
                        asset_id: asset_id.into(),
                        handle: handle.into(),
                    })
            })
            .collect()
    }
    pub fn font_handles(&self) -> Vec<ClassicTextFontHandle> {
        self.inner
            .handles()
            .filter_map(|(id, handle)| {
                id.strip_prefix("font:")
                    .map(|font_id| ClassicTextFontHandle {
                        font_id: font_id.into(),
                        handle: handle.into(),
                    })
            })
            .collect()
    }
}
impl<F: FnMut() -> Result<(), EditorError>> ClassicVisualResources
    for VerifiedClassicTextResources<F>
{
    fn resolve(
        &mut self,
        resource: &VisualResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError> {
        let size = resource
            .size
            .parse::<u64>()
            .map_err(|_| invalid("Invalid media byte count"))?;
        self.inner.resolve(
            &format!("media:{}", resource.asset_id),
            &resource.content_hash,
            size,
            handle,
        )
    }
    fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        self.inner.read(transfer, offset, count)
    }
    fn release(&mut self, transfer: &str) -> Result<(), EditorError> {
        self.inner.release(transfer)
    }
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        self.inner.checkpoint()
    }
}
impl<F: FnMut() -> Result<(), EditorError>> ClassicTextFrameResources
    for VerifiedClassicTextResources<F>
{
    fn resolve_font(
        &mut self,
        font: &TextFontResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError> {
        self.inner.checkpoint()?;
        let expected = self
            .fonts
            .get(&font.font_id)
            .ok_or_else(|| EditorError::new("NOT_AUTHORIZED", "Font is outside fixed execution"))?;
        if expected != font {
            return Err(invalid(
                "Font callback differs from the natively inspected resource",
            ));
        }
        let size = font
            .size
            .parse::<u64>()
            .map_err(|_| invalid("Invalid font byte count"))?;
        self.inner.resolve(
            &format!("font:{}", font.font_id),
            &font.content_hash,
            size,
            handle,
        )
    }
}

#[cfg(test)]
#[path = "text_resources_tests.rs"]
mod tests;
