//! Hardware knobs and faders linked to things in the project (MIDI learn).

use serde::{Deserialize, Serialize};

use crate::{AutomationTarget, EditError, Project, TrackId};

/// A MIDI controller (a knob, fader or wheel sending one CC number) driving
/// a control: anything automation can drive, or the master volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerLink {
    /// `0..16`.
    pub channel: u8,
    /// The CC number, `0..128`.
    pub controller: u8,
    /// `None` is the master, whose volume is the one thing it can drive.
    pub track: Option<TrackId>,
    pub target: AutomationTarget,
}

impl Project {
    /// Links a controller to a control. A controller drives one control and
    /// a control has one controller, so links either way are replaced.
    /// Links to tracks that are gone stay, unused, so undoing the delete
    /// brings them back to life.
    pub fn link_controller(&mut self, link: ControllerLink) -> Result<(), EditError> {
        if link.channel > 15 || link.controller > 127 {
            return Err(EditError::NothingSelected);
        }
        if let Some(track) = link.track
            && self.track(track).is_none()
        {
            return Err(EditError::NoSuchTrack(track));
        }
        self.controller_links.retain(|l| {
            (l.channel, l.controller) != (link.channel, link.controller)
                && (l.track, &l.target) != (link.track, &link.target)
        });
        self.controller_links.push(link);
        Ok(())
    }

    pub fn unlink_controller(
        &mut self,
        track: Option<TrackId>,
        target: &AutomationTarget,
    ) -> Result<(), EditError> {
        let before = self.controller_links.len();
        self.controller_links
            .retain(|l| (l.track, &l.target) != (track, target));
        if self.controller_links.len() == before {
            return Err(EditError::NothingSelected);
        }
        Ok(())
    }

    /// The link driving a control, if there is one.
    pub fn controller_for(
        &self,
        track: Option<TrackId>,
        target: &AutomationTarget,
    ) -> Option<&ControllerLink> {
        self.controller_links
            .iter()
            .find(|l| (l.track, &l.target) == (track, target))
    }

    /// The link a controller drives, if any.
    pub fn controller_link(&self, channel: u8, controller: u8) -> Option<&ControllerLink> {
        self.controller_links
            .iter()
            .find(|l| (l.channel, l.controller) == (channel, controller))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(controller: u8, track: Option<TrackId>, target: AutomationTarget) -> ControllerLink {
        ControllerLink {
            channel: 0,
            controller,
            track,
            target,
        }
    }

    #[test]
    fn one_controller_per_control_and_one_control_per_controller() {
        let mut p = Project::with_default_tracks();
        let t = p.track_ids()[0];
        p.link_controller(link(21, Some(t), AutomationTarget::Volume))
            .unwrap();
        p.link_controller(link(22, Some(t), AutomationTarget::Pan))
            .unwrap();
        // CC 21 moves to pan: volume loses it, and pan's CC 22 goes.
        p.link_controller(link(21, Some(t), AutomationTarget::Pan))
            .unwrap();
        assert_eq!(p.controller_links.len(), 1);
        assert_eq!(
            p.controller_link(0, 21).unwrap().target,
            AutomationTarget::Pan
        );
        assert!(
            p.controller_for(Some(t), &AutomationTarget::Volume)
                .is_none()
        );
        p.link_controller(link(7, None, AutomationTarget::Volume))
            .unwrap();
        assert!(p.controller_for(None, &AutomationTarget::Volume).is_some());
        p.unlink_controller(Some(t), &AutomationTarget::Pan)
            .unwrap();
        assert!(p.controller_link(0, 21).is_none());
        assert!(
            p.unlink_controller(Some(t), &AutomationTarget::Pan)
                .is_err()
        );
        assert!(
            p.link_controller(link(200, None, AutomationTarget::Volume))
                .is_err()
        );
    }
}
