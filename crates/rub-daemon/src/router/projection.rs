mod entities;
mod interaction;

pub(crate) use self::entities::{
    attach_result, attach_subject, coordinates_subject, element_subject, focused_frame_subject,
    navigation_subject, page_entity, snapshot_entity, tab_entity, tab_subject, viewport_subject,
};
pub(crate) use self::interaction::{
    ProjectionSignals, attach_interaction_projection, attach_select_projection,
};
