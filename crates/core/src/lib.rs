//! Persistent mixer intent and deterministic reconciliation. No audio or GUI dependencies.
pub mod effects;
#[cfg(test)]
mod system_sound_tests;
#[cfg(test)]
mod aux_tests;
pub mod dsp;
#[cfg(test)]
mod output_name_tests;
#[cfg(test)]
mod input_tests;
pub mod reconcile;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub type Id = Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Identity {
    pub application: String,
    pub name: String,
    pub role: String,
    pub channels: Vec<String>,
}

/// Explicit stream metadata, never names or playback duration.
pub fn system_sound_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("event") || role.eq_ignore_ascii_case("notification")
}
impl Identity {
    pub fn is_system_sound(&self) -> bool { system_sound_role(&self.role) }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StripKind {
    Stream,
    SystemSounds,
    Input,
    Group,
    Aux,
    Output,
    VirtualMic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Insert {
    pub id: Id,
    pub uri: String,
    pub bypass: bool,
    pub controls: BTreeMap<String, f32>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub id: Id,
    pub name: String,
    pub kind: StripKind,
    pub identity: Option<Identity>,
    #[serde(default)]
    pub input_key: Option<String>,
    #[serde(default)]
    pub output_key: Option<String>,
    #[serde(default)]
    pub output_name_override: Option<String>,
    #[serde(default)]
    pub output_original_name: Option<String>,
    /// Last nonzero device channel ratios, retained when the master reaches zero.
    #[serde(default)]
    pub output_balance: Vec<f32>,
    pub channels: Vec<String>,
    pub gain: f32,
    pub mute: bool,
    pub solo: bool,
    pub color: [u8; 3],
    pub inserts: Vec<Insert>,
}

impl Strip {
    pub fn default_color(kind: &StripKind) -> [u8; 3] {
        match kind {
            StripKind::Output | StripKind::VirtualMic => [35, 24, 151],
            StripKind::Aux => [119, 24, 151],
            StripKind::Input => [224, 164, 62],
            StripKind::SystemSounds => [100, 149, 237],
            _ => [91, 174, 170],
        }
    }
    pub fn new(name: impl Into<String>, kind: StripKind, channels: Vec<String>) -> Self {
        let color = Self::default_color(&kind);
        Self {
            id: Id::new_v4(),
            name: name.into(),
            kind,
            identity: None,
            input_key: None,
            output_key: None,
            output_name_override: None,
            output_original_name: None,
            output_balance: vec![],
            channels,
            gain: 1.0,
            mute: false,
            solo: false,
            color,
            inserts: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id")]
pub enum Destination {
    Bus(Id),
    Output(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tap {
    Pre,
    Post,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub id: Id,
    pub source: Id,
    pub destination: Destination,
    pub source_channel: usize,
    pub target_channel: usize,
    pub gain: f32,
    pub tap: Tap,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub revision: u64,
    pub strips: Vec<Strip>,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub key: String,
    pub name: String,
    pub channels: Vec<String>,
    #[serde(default = "unity")]
    pub volume: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub channel_volumes: Vec<f32>,
    /// Changes for every native Props notification, including a quick change back.
    #[serde(default)]
    pub control_revision: u64,
}
fn unity() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeId {
    pub epoch: String,
    pub serial: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamObservation {
    pub runtime: RuntimeId,
    pub identity: Identity,
    pub volume: f32,
    pub mute: bool,
    /// Canonical node/port pairs, not transient numeric identifiers.
    pub targets: Vec<(String, usize, usize)>,
    pub managed: Option<Id>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub streams: Vec<StreamObservation>,
    pub outputs: Vec<Output>,
    #[serde(default)]
    pub inputs: Vec<Input>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub key: String,
    pub name: String,
    pub channels: Vec<String>,
}

pub const MICROPHONE_KEY: &str = "alixer.microphone";

impl StripKind {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Output | Self::VirtualMic)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: Id,
    pub strip: Option<Id>,
    pub message: String,
    pub current: Vec<(String, usize, usize)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Edit {
    Gain {
        strip: Id,
        value: f32,
    },
    Mute {
        strip: Id,
        value: bool,
    },
    Solo {
        strip: Id,
        value: bool,
    },
    Rename {
        strip: Id,
        name: String,
    },
    ResetOutputName {
        strip: Id,
    },
    Color {
        strip: Id,
        color: [u8; 3],
    },
    ResetChannelColors,
    Reorder {
        strips: Vec<Id>,
    },
    AddBus {
        name: String,
        aux: bool,
        channels: Vec<String>,
    },
    AddAuxSend {
        source: Id,
        aux: Option<Id>,
    },
    SetAuxSend {
        source: Id,
        aux: Id,
        gain: Option<f32>,
        tap: Option<Tap>,
    },
    RemoveAuxSend {
        source: Id,
        aux: Id,
    },
    RemoveStrip {
        strip: Id,
    },
    PutRoute {
        route: Route,
    },
    RemoveRoute {
        route: Id,
    },
    AddInsert {
        strip: Id,
        uri: String,
    },
    RemoveInsert {
        strip: Id,
        insert: Id,
    },
    BypassInsert {
        strip: Id,
        insert: Id,
        bypass: bool,
    },
    SetControl {
        strip: Id,
        insert: Id,
        symbol: String,
        value: f32,
    },
    ReorderInserts {
        strip: Id,
        inserts: Vec<Id>,
    },
    Match {
        strip: Id,
        identity: Identity,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown strip or insert")]
    Unknown,
    #[error("invalid channel, gain, name, or ordering")]
    Invalid,
    #[error("routing would create a feedback cycle")]
    Cycle,
}

impl Scene {
    pub fn strip(&self, id: Id) -> Option<&Strip> {
        self.strips.iter().find(|s| s.id == id)
    }
    fn strip_mut(&mut self, id: Id) -> Result<&mut Strip, ModelError> {
        self.strips
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(ModelError::Unknown)
    }
    /// Validate edits on a copy; rejected commands never partially mutate intent.
    pub fn edit(&mut self, edit: Edit) -> Result<(), ModelError> {
        let mut next = self.clone();
        next.apply(edit)?;
        next.validate()?;
        next.revision = self.revision + 1;
        *self = next;
        Ok(())
    }
    fn apply(&mut self, edit: Edit) -> Result<(), ModelError> {
        match edit {
            Edit::Gain { strip, value } => self.strip_mut(strip)?.gain = value,
            Edit::Mute { strip, value } => self.strip_mut(strip)?.mute = value,
            Edit::Solo { strip, value } => {
                let s = self.strip_mut(strip)?;
                if !matches!(s.kind, StripKind::Stream | StripKind::Input) {
                    return Err(ModelError::Invalid);
                }
                s.solo = value;
            }
            Edit::Rename { strip, name } => {
                let s = self.strip_mut(strip)?;
                if s.kind == StripKind::Output {
                    s.output_original_name.get_or_insert_with(|| s.name.clone());
                    s.output_name_override = Some(name.clone());
                }
                s.name = name;
            }
            Edit::ResetOutputName { strip } => {
                let s = self.strip_mut(strip)?;
                if s.kind != StripKind::Output {
                    return Err(ModelError::Invalid);
                }
                s.output_name_override = None;
                if let Some(original) = &s.output_original_name {
                    s.name = original.clone();
                }
            }
            Edit::Color { strip, color } => self.strip_mut(strip)?.color = color,
            Edit::ResetChannelColors => {
                for strip in &mut self.strips {
                    strip.color = Strip::default_color(&strip.kind);
                }
            }
            Edit::Reorder { strips } => {
                let unique: BTreeSet<_> = strips.iter().copied().collect();
                if unique.len() != self.strips.len()
                    || unique.iter().any(|id| self.strip(*id).is_none())
                {
                    return Err(ModelError::Invalid);
                }
                self.strips
                    .sort_by_key(|s| strips.iter().position(|id| *id == s.id).unwrap());
            }
            Edit::AddBus {
                name,
                aux,
                channels,
            } => self.strips.push(Strip::new(
                name,
                if aux {
                    StripKind::Aux
                } else {
                    StripKind::Group
                },
                channels,
            )),
            Edit::AddAuxSend { source, aux } => {
                let input = self.strip(source).ok_or(ModelError::Unknown)?.clone();
                if input.kind.terminal() {
                    return Err(ModelError::Invalid);
                }
                let target = if let Some(id) = aux {
                    self.strip(id).ok_or(ModelError::Unknown)?.clone()
                } else {
                    let name = (1..)
                        .map(|n| format!("Aux{n}"))
                        .find(|name| {
                            !self
                                .strips
                                .iter()
                                .any(|s| s.name.eq_ignore_ascii_case(name))
                        })
                        .unwrap();
                    let channels = if input.channels.len() <= 2 {
                        vec!["FL".into(), "FR".into()]
                    } else {
                        input.channels.clone()
                    };
                    let master = Strip::new(name, StripKind::Aux, channels);
                    self.strips.push(master.clone());
                    master
                };
                if self
                    .routes
                    .iter()
                    .any(|r| r.source == source && r.destination == Destination::Bus(target.id))
                {
                    return Err(ModelError::Invalid);
                }
                for (sc, tc) in aux_send_channels(&input, &target)? {
                    self.routes.push(Route {
                        id: Id::new_v4(),
                        source,
                        destination: Destination::Bus(target.id),
                        source_channel: sc,
                        target_channel: tc,
                        gain: 0.0,
                        tap: Tap::Post,
                    });
                }
            }
            Edit::SetAuxSend {
                source,
                aux,
                gain,
                tap,
            } => {
                self.require_aux_send(source, aux)?;
                for route in self
                    .routes
                    .iter_mut()
                    .filter(|r| r.source == source && r.destination == Destination::Bus(aux))
                {
                    if let Some(gain) = gain {
                        route.gain = gain;
                    }
                    if let Some(tap) = tap {
                        route.tap = tap;
                    }
                }
            }
            Edit::RemoveAuxSend { source, aux } => {
                self.require_aux_send(source, aux)?;
                self.routes
                    .retain(|r| !(r.source == source && r.destination == Destination::Bus(aux)));
            }
            Edit::RemoveStrip { strip } => {
                if self.strip_mut(strip)?.kind.terminal() || self.strip_mut(strip)?.kind == StripKind::SystemSounds {
                    return Err(ModelError::Invalid);
                }
                self.strips.retain(|s| s.id != strip);
                self.routes
                    .retain(|r| r.source != strip && r.destination != Destination::Bus(strip));
            }
            Edit::PutRoute { route } => {
                self.routes.retain(|r| r.id != route.id);
                self.routes.push(route);
            }
            Edit::RemoveRoute { route } => self.routes.retain(|r| r.id != route),
            Edit::AddInsert { strip, uri } => self.strip_mut(strip)?.inserts.push(Insert {
                id: Id::new_v4(),
                controls: if effects::Effect::from_uri(&uri) == Some(effects::Effect::Equalizer) {
                    (0..8).map(|band| (format!("q_{band}"), effects::DEFAULT_EQ_Q)).collect()
                } else {
                    BTreeMap::new()
                },
                uri,
                bypass: false,
                state: None,
            }),
            Edit::RemoveInsert { strip, insert } => {
                self.strip_mut(strip)?.inserts.retain(|i| i.id != insert)
            }
            Edit::BypassInsert {
                strip,
                insert,
                bypass,
            } => {
                self.strip_mut(strip)?
                    .inserts
                    .iter_mut()
                    .find(|i| i.id == insert)
                    .ok_or(ModelError::Unknown)?
                    .bypass = bypass
            }
            Edit::SetControl {
                strip,
                insert,
                symbol,
                value,
            } => {
                if !value.is_finite() {
                    return Err(ModelError::Invalid);
                }
                self.strip_mut(strip)?
                    .inserts
                    .iter_mut()
                    .find(|i| i.id == insert)
                    .ok_or(ModelError::Unknown)?
                    .controls
                    .insert(symbol, value);
            }
            Edit::ReorderInserts { strip, inserts } => {
                let chain = &mut self.strip_mut(strip)?.inserts;
                let unique: BTreeSet<_> = inserts.iter().copied().collect();
                if unique.len() != chain.len() || chain.iter().any(|i| !unique.contains(&i.id)) {
                    return Err(ModelError::Invalid);
                }
                chain.sort_by_key(|i| inserts.iter().position(|id| *id == i.id).unwrap());
            }
            Edit::Match { strip, identity } => {
                let s = self.strip_mut(strip)?;
                if s.kind != StripKind::Stream || identity.channels != s.channels {
                    return Err(ModelError::Invalid);
                }
                s.identity = Some(identity);
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), ModelError> {
        let ids: BTreeSet<_> = self.strips.iter().map(|s| s.id).collect();
        if ids.len() != self.strips.len() {
            return Err(ModelError::Invalid);
        }
        if self.strips.iter().filter(|s| s.kind == StripKind::SystemSounds).count() > 1 {
            return Err(ModelError::Invalid);
        }
        let mut endpoints = BTreeSet::new();
        for s in &self.strips {
            if s.kind == StripKind::SystemSounds && (s.identity.is_some() || s.solo || !s.inserts.is_empty()) {
                return Err(ModelError::Invalid);
            }
            if s.output_balance
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            {
                return Err(ModelError::Invalid);
            }
            if s.channels.is_empty()
                || s.channels.len() > 64
                || s.name.trim().is_empty()
                || !valid_gain(s.gain)
            {
                return Err(ModelError::Invalid);
            }
            if s.kind.terminal() {
                if s.output_name_override
                    .as_ref()
                    .is_some_and(|name| name != &s.name)
                {
                    return Err(ModelError::Invalid);
                }
                let key = s
                    .output_key
                    .as_ref()
                    .filter(|k| !k.is_empty())
                    .ok_or(ModelError::Invalid)?;
                if !endpoints.insert(key) || s.solo {
                    return Err(ModelError::Invalid);
                }
            } else if s.output_key.is_some()
                || s.output_name_override.is_some()
                || s.output_original_name.is_some()
            {
                return Err(ModelError::Invalid);
            }
            if (s.kind == StripKind::Input) != s.input_key.is_some()
                || s.input_key.as_ref().is_some_and(|key| key.is_empty())
                || (s.kind == StripKind::VirtualMic
                    && s.output_key.as_deref() != Some(MICROPHONE_KEY))
            {
                return Err(ModelError::Invalid);
            }
        }
        let mut route_ids = BTreeSet::new();
        for r in &self.routes {
            if !route_ids.insert(r.id) || !valid_gain(r.gain) {
                return Err(ModelError::Invalid);
            }
            let source = self.strip(r.source).ok_or(ModelError::Unknown)?;
            if source.kind.terminal() || source.kind == StripKind::SystemSounds
                || r.source_channel >= source.channels.len()
                || r.target_channel >= 64
            {
                return Err(ModelError::Invalid);
            }
            if let Destination::Bus(id) = r.destination {
                let bus = self.strip(id).ok_or(ModelError::Unknown)?;
                if !matches!(bus.kind, StripKind::Group | StripKind::Aux)
                    || r.target_channel >= bus.channels.len()
                {
                    return Err(ModelError::Invalid);
                }
            }
        }
        self.order().map(|_| ())
    }
    fn require_aux_send(&self, source: Id, aux: Id) -> Result<(), ModelError> {
        let input = self.strip(source).ok_or(ModelError::Unknown)?;
        let target = self.strip(aux).ok_or(ModelError::Unknown)?;
        if input.kind.terminal() || input.kind == StripKind::SystemSounds || target.kind != StripKind::Aux {
            return Err(ModelError::Invalid);
        }
        if !self
            .routes
            .iter()
            .any(|r| r.source == source && r.destination == Destination::Bus(aux))
        {
            return Err(ModelError::Unknown);
        }
        Ok(())
    }
    pub fn order(&self) -> Result<Vec<Id>, ModelError> {
        let mut remaining: BTreeSet<_> = self.strips.iter().map(|s| s.id).collect();
        let mut result = vec![];
        while !remaining.is_empty() {
            let ready = remaining.iter().copied().find(|id| {
                !self.routes.iter().any(|r| {
                    self.route_target(&r.destination) == Some(*id) && remaining.contains(&r.source)
                })
            });
            let id = ready.ok_or(ModelError::Cycle)?;
            remaining.remove(&id);
            result.push(id);
        }
        Ok(result)
    }
    pub fn audible(&self, strip: &Strip) -> bool {
        !strip.mute
            && (!matches!(strip.kind, StripKind::Stream | StripKind::Input)
                || !self.strips.iter().any(|s| s.solo)
                || strip.solo)
    }
    pub fn route_target(&self, destination: &Destination) -> Option<Id> {
        match destination {
            Destination::Bus(id) => Some(*id),
            Destination::Output(key) => self
                .strips
                .iter()
                .find(|s| s.output_key.as_ref() == Some(key))
                .map(|s| s.id),
        }
    }
}

pub fn aux_send_channels(
    source: &Strip,
    target: &Strip,
) -> Result<Vec<(usize, usize)>, ModelError> {
    if source.kind.terminal() || source.kind == StripKind::SystemSounds || target.kind != StripKind::Aux || source.id == target.id {
        return Err(ModelError::Invalid);
    }
    if source.channels.len() == 1 && target.channels.len() <= 2 {
        return Ok((0..target.channels.len()).map(|tc| (0, tc)).collect());
    }
    if source.channels.len() != target.channels.len() {
        return Err(ModelError::Invalid);
    }
    let named = source.channels.iter().collect::<BTreeSet<_>>().len() == source.channels.len()
        && target.channels.iter().all(|c| source.channels.contains(c));
    Ok(target
        .channels
        .iter()
        .enumerate()
        .map(|(tc, name)| {
            (
                if named {
                    source.channels.iter().position(|c| c == name).unwrap()
                } else {
                    tc
                },
                tc,
            )
        })
        .collect())
}

fn valid_gain(value: f32) -> bool {
    value.is_finite() && (0.0..=4.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_masters_are_terminal_and_cannot_be_deleted_or_soloed() {
        let mut scene = Scene::default();
        let mut output = Strip::new("Speakers", StripKind::Output, vec!["MONO".into()]);
        output.output_key = Some("speakers".into());
        let id = output.id;
        scene.strips.push(output);
        scene
            .edit(Edit::AddBus {
                name: "Mix".into(),
                aux: false,
                channels: vec!["MONO".into()],
            })
            .unwrap();
        let bus = scene.strips[1].id;
        for edit in [
            Edit::Solo {
                strip: id,
                value: true,
            },
            Edit::RemoveStrip { strip: id },
            Edit::PutRoute {
                route: Route {
                    id: Id::new_v4(),
                    source: id,
                    destination: Destination::Bus(bus),
                    source_channel: 0,
                    target_channel: 0,
                    gain: 1.0,
                    tap: Tap::Post,
                },
            },
            Edit::PutRoute {
                route: Route {
                    id: Id::new_v4(),
                    source: bus,
                    destination: Destination::Bus(id),
                    source_channel: 0,
                    target_channel: 0,
                    gain: 1.0,
                    tap: Tap::Post,
                },
            },
        ] {
            let before = scene.clone();
            assert!(scene.edit(edit).is_err());
            assert_eq!(scene, before);
        }
    }
    #[test]
    fn scenes_without_output_fields_remain_readable() {
        let mut scene = Scene::default();
        scene
            .strips
            .push(Strip::new("Old bus", StripKind::Group, vec!["MONO".into()]));
        let mut json = serde_json::to_value(&scene).unwrap();
        json["strips"][0]
            .as_object_mut()
            .unwrap()
            .remove("output_key");
        json["strips"][0]
            .as_object_mut()
            .unwrap()
            .remove("output_balance");
        for key in ["output_name_override", "output_original_name"] {
            json["strips"][0].as_object_mut().unwrap().remove(key);
        }
        let loaded: Scene = serde_json::from_value(json).unwrap();
        loaded.validate().unwrap();
        assert_eq!(scene, loaded);
    }
    #[test]
    fn feedback_edit_is_atomic() {
        let mut scene = Scene::default();
        for n in ["A", "B"] {
            scene
                .strips
                .push(Strip::new(n, StripKind::Group, vec!["MONO".into()]));
        }
        let (a, b) = (scene.strips[0].id, scene.strips[1].id);
        let route = |source, target| Route {
            id: Id::new_v4(),
            source,
            destination: Destination::Bus(target),
            source_channel: 0,
            target_channel: 0,
            gain: 1.0,
            tap: Tap::Post,
        };
        scene.edit(Edit::PutRoute { route: route(a, b) }).unwrap();
        let before = scene.clone();
        assert!(matches!(
            scene.edit(Edit::PutRoute { route: route(b, a) }),
            Err(ModelError::Cycle)
        ));
        assert_eq!(scene, before);
    }
}
