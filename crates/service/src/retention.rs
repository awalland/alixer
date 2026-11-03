//! Playback history is retained for an hour; explicit user settings are durable.
use crate::store::Saved;
use alixer_core::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SWEEP_SECONDS: u64 = 60;
pub const RETENTION_SECONDS: u64 = 60 * 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Retention {
    #[serde(default)]
    pub absent_since: BTreeMap<Id, u64>,
    #[serde(default)]
    pub protected: BTreeSet<Id>,
    #[serde(default)]
    pub history_imported: bool,
}

impl Retention {
    /// A desktop audio-control move is a user routing edit too. Route IDs can
    /// change during reconciliation, so compare their actual signal paths.
    pub fn protect_changed_routes(&mut self, before: &Scene, after: &Scene) {
        let routes = |scene: &Scene, id| {
            let mut routes: Vec<_> = scene
                .routes
                .iter()
                .filter(|r| r.source == id)
                .map(|r| (r.destination.clone(), r.source_channel, r.target_channel, r.gain, r.tap))
                .collect();
            routes.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
            routes
        };
        for strip in before.strips.iter().filter(|s| s.kind == StripKind::Stream) {
            if after.strip(strip.id).is_some() && routes(before, strip.id) != routes(after, strip.id) {
                self.protected.insert(strip.id);
            }
        }
    }

    /// Remember explicit edits even when a channel is later reset to defaults.
    /// Route removal must be recorded against the scene *before* the edit.
    pub fn protect_edit(&mut self, scene: &Scene, edit: &Edit) {
        let id = match edit {
            Edit::Gain { strip, .. }
            | Edit::Mute { strip, .. }
            | Edit::Solo { strip, .. }
            | Edit::Rename { strip, .. }
            | Edit::Color { strip, .. }
            | Edit::AddInsert { strip, .. }
            | Edit::RemoveInsert { strip, .. }
            | Edit::BypassInsert { strip, .. }
            | Edit::SetControl { strip, .. }
            | Edit::ReorderInserts { strip, .. }
            | Edit::Match { strip, .. } => Some(*strip),
            Edit::AddAuxSend { source, .. } | Edit::SetAuxSend { source, .. } | Edit::RemoveAuxSend { source, .. } => Some(*source),
            Edit::PutRoute { route } => Some(route.source),
            Edit::RemoveRoute { route } => scene.routes.iter().find(|r| r.id == *route).map(|r| r.source),
            // Ordering and resetting the palette do not customize a channel's mix.
            _ => None,
        };
        if let Some(id) = id.filter(|id| scene.strip(*id).is_some_and(|s| s.kind == StripKind::Stream)) {
            self.protected.insert(id);
        }
    }
}

/// Called for each authoritative observation, including streams that are paused.
pub fn observe(saved: &mut Saved, present: &BTreeSet<Id>, now: u64) {
    let streams: BTreeSet<_> = saved.scene.strips.iter().filter(|s| s.kind == StripKind::Stream).map(|s| s.id).collect();
    saved.retention.absent_since.retain(|id, _| streams.contains(id) && !present.contains(id));
    saved.retention.protected.retain(|id| streams.contains(id));
    for id in streams.difference(present) {
        saved.retention.absent_since.entry(*id).or_insert(now);
    }
}

fn default_playback(scene: &Scene, s: &Strip) -> bool {
    if s.kind != StripKind::Stream
        || s.gain != 1.0
        || s.mute
        || s.solo
        || !s.inserts.is_empty()
        || s.color != Strip::default_color(&StripKind::Stream)
        || !s.identity.as_ref().is_some_and(|identity| identity.name == s.name && identity.channels == s.channels)
    {
        return false;
    }
    let routes: Vec<_> = scene.routes.iter().filter(|r| r.source == s.id).collect();
    // Be conservative with legacy sessions: only a complete, straight-through
    // unity route to one device is eligible. Keep disconnected/custom matrices.
    routes.len() == s.channels.len()
        && !routes.is_empty()
        && routes.iter().all(|r| {
            matches!(r.destination, Destination::Output(_)) && r.destination == routes[0].destination && r.gain == 1.0 && r.tap == Tap::Post && r.source_channel == r.target_channel
        })
        && (0..s.channels.len()).all(|ch| routes.iter().any(|r| r.source_channel == ch))
        && !scene.routes.iter().any(|r| r.destination == Destination::Bus(s.id))
}

pub fn sweep(saved: &mut Saved, now: u64) -> Result<usize, ModelError> {
    let expired: BTreeSet<_> = saved
        .scene
        .strips
        .iter()
        .filter(|s| {
            saved.retention.absent_since.get(&s.id).is_some_and(|since| now.saturating_sub(*since) >= RETENTION_SECONDS)
                && !saved.retention.protected.contains(&s.id)
                && !saved.conflicts.iter().any(|c| c.strip == Some(s.id))
                && default_playback(&saved.scene, s)
        })
        .map(|s| s.id)
        .collect();
    if expired.is_empty() {
        return Ok(0);
    }
    let mut scene = saved.scene.clone();
    scene.strips.retain(|s| !expired.contains(&s.id));
    scene
        .routes
        .retain(|r| !expired.contains(&r.source) && !matches!(r.destination, Destination::Bus(id) if expired.contains(&id)));
    scene.revision += 1;
    scene.validate()?;
    saved.scene = scene;
    saved.checkpoint.bindings.retain(|_, id| !expired.contains(id));
    saved.retention.absent_since.retain(|id, _| !expired.contains(id));
    saved.retention.protected.retain(|id| !expired.contains(id));
    Ok(expired.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Saved, Id) {
        let mut strip = Strip::new("browser", StripKind::Stream, vec!["FL".into(), "FR".into()]);
        strip.identity = Some(Identity {
            name: strip.name.clone(),
            channels: strip.channels.clone(),
            ..Default::default()
        });
        let id = strip.id;
        let mut saved = Saved::default();
        saved.scene.strips.push(strip);
        alixer_core::reconcile::replace_outputs(&mut saved.scene, id, &[("speakers".into(), 0, 0), ("speakers".into(), 1, 1)]);
        (saved, id)
    }

    #[test]
    fn expires_after_full_hour_and_removes_routes_with_new_revision() {
        let (mut saved, id) = fixture();
        observe(&mut saved, &BTreeSet::new(), 100);
        assert_eq!(sweep(&mut saved, 3699).unwrap(), 0);
        assert_eq!(sweep(&mut saved, 50).unwrap(), 0, "clock reversal cannot expire a channel");
        let revision = saved.scene.revision;
        assert_eq!(sweep(&mut saved, 3700).unwrap(), 1);
        assert!(saved.scene.strip(id).is_none());
        assert!(saved.scene.routes.is_empty());
        assert!(saved.retention.absent_since.is_empty());
        assert_eq!(saved.scene.revision, revision + 1);
        assert_eq!(sweep(&mut saved, 10000).unwrap(), 0);
    }

    #[test]
    fn present_or_paused_streams_reset_absence_and_timestamps_survive_restart() {
        let (mut saved, id) = fixture();
        observe(&mut saved, &BTreeSet::new(), 100);
        observe(&mut saved, &BTreeSet::from([id]), 3699);
        assert_eq!(sweep(&mut saved, 9000).unwrap(), 0);
        observe(&mut saved, &BTreeSet::new(), 9000);
        let mut restored: Saved = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        observe(&mut restored, &BTreeSet::new(), 10000);
        assert_eq!(sweep(&mut restored, 12599).unwrap(), 0);
        assert_eq!(sweep(&mut restored, 12600).unwrap(), 1);
    }

    #[test]
    fn customized_channels_conflicts_and_non_playback_are_preserved() {
        let (base, id) = fixture();
        for kind in 0..11 {
            let mut saved = base.clone();
            let strip = &mut saved.scene.strips[0];
            match kind {
                0 => strip.gain = 0.5,
                1 => strip.mute = true,
                2 => strip.solo = true,
                3 => strip.name = "my browser".into(),
                4 => strip.color = [1, 2, 3],
                5 => strip.inserts.push(Insert {
                    id: Id::new_v4(),
                    uri: "test".into(),
                    bypass: true,
                    controls: BTreeMap::new(),
                    state: None,
                }),
                6 => saved.scene.routes[0].gain = 0.5,
                7 => saved.scene.routes[0].tap = Tap::Pre,
                8 => saved.scene.routes.clear(),
                9 => saved.scene.routes[0].destination = Destination::Output("headphones".into()),
                _ => saved.conflicts.push(Conflict {
                    id: Id::new_v4(),
                    strip: Some(id),
                    message: "routing".into(),
                    current: vec![],
                }),
            }
            observe(&mut saved, &BTreeSet::new(), 0);
            assert_eq!(sweep(&mut saved, 7200).unwrap(), 0, "customization {kind}");
        }
        for kind in [
            StripKind::Input,
            StripKind::Output,
            StripKind::VirtualMic,
            StripKind::Group,
            StripKind::Aux,
            StripKind::SystemSounds,
        ] {
            let mut saved = base.clone();
            saved.scene.strips[0].kind = kind;
            observe(&mut saved, &BTreeSet::new(), 0);
            assert_eq!(sweep(&mut saved, 7200).unwrap(), 0);
        }
    }

    #[test]
    fn explicit_routing_and_reset_to_defaults_remain_protected() {
        let (mut saved, id) = fixture();
        let edit = Edit::RemoveRoute { route: saved.scene.routes[0].id };
        saved.retention.protect_edit(&saved.scene, &edit);
        // Even if the user restores exactly the default route afterwards.
        observe(&mut saved, &BTreeSet::new(), 0);
        assert_eq!(sweep(&mut saved, 7200).unwrap(), 0);
        assert!(saved.retention.protected.contains(&id));
    }

    #[test]
    fn desktop_moves_protect_routes_but_new_route_ids_do_not() {
        let (mut saved, id) = fixture();
        let mut after = saved.scene.clone();
        for route in &mut after.routes {
            route.id = Id::new_v4();
        }
        saved.retention.protect_changed_routes(&saved.scene, &after);
        assert!(saved.retention.protected.is_empty());
        for route in &mut after.routes {
            route.destination = Destination::Output("headphones".into());
        }
        saved.retention.protect_changed_routes(&saved.scene, &after);
        assert!(saved.retention.protected.contains(&id));
    }
}
