use super::*;
use opencut_api_contracts::{
    blend_frame_plan::BlendFramePlan, caption_frame_plan::CaptionFramePlan,
    graphic_frame_plan::GraphicFramePlan, mask_frame_plan::MaskFramePlan,
    scene_frame_plan::SceneFramePlan, text_frame_plan::TextFramePlan,
    visual_preview::ClassicVisualPlan,
};
use serde_json::Value;

/// Existing wire formats describe stored semantic facts. They select no
/// execution backend: every branch below enters the same native compositor.
pub(super) enum Plan {
    Media(ClassicVisualPlan),
    Text(TextFramePlan),
    Caption(CaptionFramePlan),
    Graphic(GraphicFramePlan),
    Blend(BlendFramePlan),
    Mask(MaskFramePlan),
    Scene(SceneFramePlan),
}
impl Plan {
    pub fn parse(kind: &str, value: Value) -> Result<Self, EditorError> {
        macro_rules! parse {
            ($variant:ident) => {
                Self::$variant(
                    serde_json::from_value(value).map_err(|_| {
                        invalid("Native plan differs from its typed frame declaration")
                    })?,
                )
            };
        }
        Ok(match kind {
            "begin" => parse!(Media),
            "beginTextFrame" => parse!(Text),
            "beginCaptionFrame" => parse!(Caption),
            "beginGraphicFrame" => parse!(Graphic),
            "beginBlendFrame" => parse!(Blend),
            "beginMaskFrame" => parse!(Mask),
            "beginSceneFrame" => parse!(Scene),
            _ => return Err(invalid("Unknown native frame plan")),
        })
    }
    pub fn media(&self) -> &ClassicVisualPlan {
        match self {
            Self::Media(plan) => plan,
            _ => &self.text().expect("text child").media,
        }
    }
    pub fn text(&self) -> Option<&TextFramePlan> {
        match self {
            Self::Text(plan) => Some(plan),
            _ => self.caption().map(|plan| &plan.execution),
        }
    }
    pub fn caption(&self) -> Option<&CaptionFramePlan> {
        match self {
            Self::Caption(plan) => Some(plan),
            _ => self.graphic().map(|plan| &plan.execution),
        }
    }
    pub fn graphic(&self) -> Option<&GraphicFramePlan> {
        match self {
            Self::Graphic(plan) => Some(plan),
            _ => self.blend().map(|plan| &plan.execution),
        }
    }
    pub fn blend(&self) -> Option<&BlendFramePlan> {
        match self {
            Self::Blend(plan) => Some(plan),
            Self::Mask(plan) => Some(&plan.execution),
            Self::Scene(plan) => Some(&plan.execution),
            _ => None,
        }
    }
    pub fn mask(&self) -> Option<&MaskFramePlan> {
        if let Self::Mask(plan) = self {
            Some(plan)
        } else {
            None
        }
    }
    pub fn scene(&self) -> Option<&SceneFramePlan> {
        if let Self::Scene(plan) = self {
            Some(plan)
        } else {
            None
        }
    }
}
