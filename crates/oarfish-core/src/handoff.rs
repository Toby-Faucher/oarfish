//! The pipeline-to-engine handoff: one classified line.
//!
//! The M3 pipeline owns the mask bundle and the Drain table, and the engine
//! owns the windows and the alarm state machine. Neither owns the other's
//! table, so the pipeline forwards everything the engine needs in one value:
//! the [`Event`], the [`TemplateId`] Drain assigned, and the masked cluster
//! text at train time, which is what the verdict cache judges and keys on.
//!
//! The template text is a snapshot: Drain may later generalize the cluster,
//! which moves the id with it. A generalized line arrives as a new id and is
//! judged under that id, never aliased to the old one.

use serde::{Deserialize, Serialize};

use crate::{Event, TemplateId};

/// One line, masked and clustered, ready for the verdict gate and the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInput {
    pub event: Event,
    pub template_id: TemplateId,
    /// The masked cluster text Drain assigned this line to, at train time.
    pub template: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use bytes::Bytes;

    #[test]
    fn the_handoff_carries_event_id_and_template_together() {
        let input = EngineInput {
            event: Event::new(
                Bytes::from_static(b"EXT4-fs error"),
                "nas01",
                Source::Syslog,
            ),
            template_id: TemplateId::of("EXT4-fs error"),
            template: "EXT4-fs error".to_owned(),
        };
        assert_eq!(input.template_id, TemplateId::of(&input.template));
        assert_eq!(input.event.host, "nas01");
    }
}
