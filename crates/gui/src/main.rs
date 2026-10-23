mod aux_sends;
mod conflict_ui;
mod dev_service;
mod effect_editor;
mod eq_curve;
mod fader;
#[cfg(test)]
mod ipc_tests;
mod plugins;
mod preferences;
mod strip_actions;
mod strip_order;
mod strip_routing;
use alixer_core::*;
use alixer_protocol::*;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    time::Duration,
};

const BG: Color32 = Color32::from_rgb(17, 20, 24);
const PANEL: Color32 = Color32::from_rgb(26, 30, 35);
const MINT: Color32 = Color32::from_rgb(110, 214, 182);
const DIM: Color32 = Color32::from_rgb(144, 155, 167);
const RED: Color32 = Color32::from_rgb(245, 115, 108);
const STRIP_MIN_HEIGHT: f32 = 656.0;
const STRIP_NAME_HEIGHT: f32 = 64.0;

fn output_masters(snapshot: &Snapshot, show_offline: bool) -> impl Iterator<Item = &Strip> {
    snapshot.scene.strips.iter().filter(move |s| {
        s.kind.terminal()
            && (s.kind == StripKind::VirtualMic || show_offline
                || snapshot
                    .observation
                    .outputs
                    .iter()
                    .any(|o| Some(&o.key) == s.output_key.as_ref()))
    })
}
enum Event {
    Snapshot(Snapshot),
    Error(String),
    Catalog(Result<serde_json::Value, String>),
}
struct App {
    tx: mpsc::Sender<Command>,
    rx: mpsc::Receiver<Event>,
    snapshot: Option<Snapshot>,
    error: String,
    selected: Option<Id>,
    new_bus: Option<bool>,
    bus_name: String,
    catalog: serde_json::Value,
    catalog_requested: bool,
    catalog_loading: bool,
    catalog_error: String,
    plugin_picker: Option<Id>,
    selected_insert: Option<(Id, Id)>,
    rename_strip: Option<(Id, String)>,
    confirm_output_names: bool,
    filter: String,
    show_inactive: bool,
    strip_presence: BTreeMap<Id, Option<f64>>,
    names: BTreeMap<Id, String>,
    preferences: preferences::Preferences,
}
fn exchange(
    stream: &mut UnixStream,
    command: Command,
    revision: Option<u64>,
) -> Result<Reply, String> {
    let frame_limit = reply_frame_limit(&command);
    serde_json::to_writer(
        &mut *stream,
        &Request {
            version: VERSION,
            id: 1,
            revision,
            command,
        },
    )
    .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream.try_clone().map_err(|e| e.to_string())?)
        .take((frame_limit + 1) as u64)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > frame_limit {
        return Err("Service response exceeded limit".into());
    }
    let reply: Reply = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if reply.version != VERSION {
        return Err(format!(
            "Service protocol {} is incompatible with GUI protocol {VERSION}; restart alixerd after rebuilding",
            reply.version
        ));
    }
    Ok(reply)
}
fn fetch_catalog(mut stream: UnixStream) -> Result<serde_json::Value, String> {
    stream
        .set_read_timeout(Some(PLUGIN_SCAN_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    match exchange(&mut stream, Command::Plugins, None)?.result? {
        Payload::Plugins(value) => Ok(value),
        _ => Err("Unexpected plugin catalog response".into()),
    }
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let preferences = preferences::Preferences::load(state_dir().join("ui.json"));
        cc.egui_ctx.set_zoom_factor(preferences.zoom);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, |s| {
            s.visuals.panel_fill = BG;
            s.visuals.window_fill = PANEL;
            s.visuals.selection.bg_fill = Color32::from_rgb(45, 92, 80);
            s.visuals.widgets.active.bg_fill = MINT;
            s.spacing.item_spacing = Vec2::new(9.0, 9.0);
        });
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let mut revision = None;
            let mut socket: Option<UnixStream> = None;
            loop {
                if socket.is_none() {
                    match UnixStream::connect(runtime_dir().join("control.sock")) {
                        Ok(s) => {
                            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                            socket = Some(s);
                        }
                        Err(e) => {
                            if events
                                .send(Event::Error(format!("Service unavailable · {e}")))
                                .is_err()
                            {
                                return;
                            }
                            ctx.request_repaint();
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    }
                }
                let stream = socket.as_mut().unwrap();
                let mut failure = None;
                for command in commands.try_iter() {
                    if matches!(command, Command::Plugins) {
                        let events = events.clone();
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let result = UnixStream::connect(runtime_dir().join("control.sock"))
                                .map_err(|e| format!("Service unavailable · {e}"))
                                .and_then(fetch_catalog);
                            let _ = events.send(Event::Catalog(result));
                            ctx.request_repaint();
                        });
                        continue;
                    }
                    match exchange(stream, command, revision) {
                        Ok(reply) => {
                            revision = Some(reply.revision);
                            match reply.result {
                                Err(e) => {
                                    let _ = events.send(Event::Error(e));
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    }
                }
                if failure.is_none() {
                    match exchange(stream, Command::Snapshot, None) {
                        Ok(reply) => {
                            revision = Some(reply.revision);
                            if let Ok(Payload::Snapshot(snapshot)) = reply.result {
                                if events.send(Event::Snapshot(snapshot)).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => failure = Some(e),
                    }
                }
                if let Some(e) = failure {
                    let _ = events.send(Event::Error(e));
                    socket = None;
                }
                ctx.request_repaint();
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            tx,
            rx,
            snapshot: None,
            error: String::new(),
            selected: None,
            new_bus: None,
            bus_name: String::new(),
            catalog: serde_json::Value::Null,
            catalog_requested: false,
            catalog_loading: false,
            catalog_error: String::new(),
            plugin_picker: None,
            selected_insert: None,
            rename_strip: None,
            confirm_output_names: false,
            filter: String::new(),
            show_inactive: false,
            strip_presence: BTreeMap::new(),
            names: BTreeMap::new(),
            preferences,
        }
    }
    fn send(&mut self, command: Command) {
        if matches!(command, Command::Plugins) {
            if self.catalog_loading {
                return;
            }
            self.catalog_requested = true;
            self.catalog_loading = true;
            self.catalog_error.clear();
        }
        self.error.clear();
        if let Err(e) = self.tx.send(command) {
            self.error = e.to_string();
        }
    }
    fn edit(&mut self, edit: Edit) {
        self.send(Command::Edit(edit));
    }
    fn console(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.horizontal(|ui| {
            ui.heading("Mix console");
            self.conflict_badge(ui, snapshot, None);
            ui.label(
                RichText::new(format!(
                    "{} inputs · {} streams · {} buses · {} outputs",
                    snapshot.observation.inputs.len(),
                    snapshot.observation.streams.len(),
                    snapshot
                        .scene
                        .strips
                        .iter()
                        .filter(|s| matches!(s.kind, StripKind::Group | StripKind::Aux))
                        .count(),
                    snapshot.observation.outputs.len()
                ))
                .color(DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.show_inactive, "Show offline");
            });
        });
        ui.add_space(12.0);
        if snapshot.scene.strips.is_empty() {
            ui.add_space(90.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Your sound, on your terms.").size(26.0));
                ui.label(
                    RichText::new(
                        "Playback streams appear here as soon as PipeWire registers them.",
                    )
                    .color(DIM),
                );
                ui.add_space(12.0);
                ui.label("Start an application playing audio, or create a bus.");
            });
            return;
        }
        let strip_height =
            (ui.available_height() - ui.spacing().scroll.allocated_width()).max(STRIP_MIN_HEIGHT);
        egui::ScrollArea::both()
            .id_salt("strip-bank")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.strip_bank(ui, snapshot, strip_height);
            });
    }
    fn strip(
        &mut self,
        ui: &mut egui::Ui,
        strip: &Strip,
        snapshot: &Snapshot,
        height: f32,
    ) -> egui::Rect {
        if self.preferences.is_hidden(strip.id) {
            return self.collapsed_strip(ui, strip, snapshot, height);
        }
        let selected = self.selected == Some(strip.id);
        let output = strip.kind.terminal();
        let online = if strip.kind == StripKind::Input {
            snapshot.observation.inputs.iter().any(|i| Some(&i.key) == strip.input_key.as_ref())
        } else { strip.kind != StripKind::Output
            || snapshot
                .observation
                .outputs
                .iter()
                .any(|o| Some(&o.key) == strip.output_key.as_ref()) };
        let frame = egui::Frame::new()
            .fill(PANEL)
            .stroke(egui::Stroke::new(
                if selected { 1.5 } else { 0.5 },
                if selected {
                    MINT
                } else {
                    Color32::from_gray(55)
                },
            ))
            .inner_margin(12.0)
            .corner_radius(8.0);
        let margins = frame.total_margin().sum();
        frame.show(ui,|ui|{
            // A frame inherits the horizontal strip bank's layout.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min),|ui|{
            ui.set_width(162.0-margins.x);
            ui.set_height(height-margins.y);
            let background=ui.interact(ui.max_rect(),ui.id().with(("strip-background",strip.id)),egui::Sense::click());
            let color=Color32::from_rgb(strip.color[0],strip.color[1],strip.color[2]);
            let(rect,_)=ui.allocate_exact_size(Vec2::new(ui.available_width(),3.0),egui::Sense::hover());ui.painter().rect_filled(rect,2.0,color);
            ui.horizontal(|ui| {
                ui.set_min_height(ui.spacing().interact_size.y);
                ui.label(RichText::new(match strip.kind{StripKind::Stream=>"PLAYBACK",StripKind::SystemSounds=>"SYSTEM SOUNDS",StripKind::Input=>if online{"MIC / INPUT"}else{"INPUT · OFFLINE"},StripKind::VirtualMic=>"VIRTUAL MIC",StripKind::Group=>"GROUP",StripKind::Aux=>"AUX",StripKind::Output=>if online{"OUTPUT"}else{"OUTPUT · OFFLINE"}}).small().color(DIM));
                self.conflict_badge(ui, snapshot, Some(strip.id));
            });
            self.routing_rack(ui,strip,snapshot);
            if strip.kind == StripKind::SystemSounds {
                ui.allocate_space(Vec2::new(ui.available_width(), 64.0));
                ui.allocate_space(Vec2::new(ui.available_width(), ((height-460.0)*0.45).clamp(96.0,360.0)));
            } else {
                self.aux_rack(ui,strip,snapshot);
                self.plugin_rack(ui,strip,((height-460.0)*0.45).clamp(96.0,360.0));
            }
            ui.horizontal(|ui|{
                let mut mute=strip.mute;if ui.add_enabled(online,egui::Button::selectable(mute,RichText::new("M").color(if strip.mute{RED}else{DIM}))).clicked(){mute=!mute;self.edit(Edit::Mute{strip:strip.id,value:mute});}
                if matches!(strip.kind,StripKind::Stream|StripKind::Input){let mut solo=strip.solo;if ui.toggle_value(&mut solo,RichText::new("S").color(if strip.solo{MINT}else{DIM})).changed(){self.edit(Edit::Solo{strip:strip.id,value:solo});}}
            });
            ui.add_space(6.0);
            let footer_height=STRIP_NAME_HEIGHT+2.0*ui.spacing().interact_size.y+2.0*ui.text_style_height(&egui::TextStyle::Small)+5.0*ui.spacing().item_spacing.y;
            let fader_height=(ui.available_height()-footer_height).max(60.0);
            ui.horizontal(|ui|{
                ui.spacing_mut().slider_width=fader_height;
                let mut db=if strip.gain<=0.00001{-60.0}else{20.0*strip.gain.log10()};
                let response=ui.add_enabled(online,fader::Fader::new(&mut db,strip.id));
                if response.changed(){self.edit(Edit::Gain{strip:strip.id,value:if db<=-60.0{0.0}else{10.0_f32.powf(db/20.0)}});}
                let peaks=if output{snapshot.channel_peaks.iter().find(|(id,_)|*id==strip.id).map(|(_,p)|p.clone()).unwrap_or_else(||vec![0.0;strip.channels.len()])}else{vec![snapshot.peaks.iter().find(|(id,_)|*id==strip.id).map(|(_,p)|*p).unwrap_or_default()]};
                let meter_max=(ui.available_width()-32.0-ui.spacing().item_spacing.x).max(15.0);
                let width=if output{(peaks.len()as f32*10.0).clamp(15.0,80.0).min(meter_max)}else{15.0};
                let(rect,response)=ui.allocate_exact_size(Vec2::new(width,fader_height),egui::Sense::hover());
                let levels=peaks.iter().map(|p|if *p<=0.0{"−∞ dBFS".into()}else{format!("{:.1} dBFS",20.0*p.log10())}).collect::<Vec<String>>().join(" / ");
                response.on_hover_text(if strip.kind==StripKind::SystemSounds{format!("Loudest active system sound\n{levels}")}else if strip.kind==StripKind::Output{format!("{}\n{}\nAfter inserts, before device volume/mute",strip.channels.join(" / "),levels)}else{format!("Post-fader signal\n{levels}")});
                for(ch,peak)in peaks.iter().enumerate(){
                    let mut meter=rect;let step=width/peaks.len().max(1)as f32;meter.min.x+=ch as f32*step;meter.max.x=meter.min.x+(step-1.0).max(0.5);
                    ui.painter().rect_filled(meter,2.0,Color32::from_gray(12));
                    meter.min.y=fader::db_y(rect,20.0*peak.max(0.001).log10());
                    meter.max.y=fader::db_y(rect,-60.0);
                    ui.painter().rect_filled(meter,2.0,if *peak>=1.0{RED}else{MINT});
                }
                let(scale,_)=ui.allocate_exact_size(Vec2::new(32.0,fader_height),egui::Sense::hover());
                for(db,label)in [(12.0,"+12"),(0.0,"0"),(-12.0,"−12"),(-24.0,"−24"),(-48.0,"−48"),(-60.0,"−∞")]{
                    let y=fader::db_y(scale,db);
                    ui.painter().text(egui::pos2(scale.left(),y.clamp(scale.top()+6.0,scale.bottom()-6.0)),egui::Align2::LEFT_CENTER,label,egui::FontId::proportional(11.0),DIM);
                }
            });
            let mut db=if strip.gain<=0.0{-60.0}else{20.0*strip.gain.log10()};
            if ui.add_enabled(online,egui::DragValue::new(&mut db).range(-60.0..=12.0).speed(0.1).suffix(" dB")).changed(){self.edit(Edit::Gain{strip:strip.id,value:if db<=-60.0{0.0}else{10.0_f32.powf(db/20.0)}});}
            ui.add(egui::Label::new(RichText::new(strip.channels.join(" / ")).small().color(DIM)).truncate()).on_hover_text(strip.channels.join(" / "));
            if strip.kind == StripKind::SystemSounds {
                ui.label(RichText::new("Shared volume · mute").small().color(DIM));
            } else {
            ui.add(egui::Label::new(RichText::new(format!("{} routes · {} inserts",snapshot.scene.routes.iter().filter(|r|if output{matches!(&r.destination,Destination::Output(key)if Some(key)==strip.output_key.as_ref())}else{r.source==strip.id}).count(),strip.inserts.len())).small().color(DIM)).truncate());
            }
            let name_top=ui.max_rect().bottom()-STRIP_NAME_HEIGHT;
            ui.add_space((name_top-ui.cursor().top()).max(0.0));
            egui::ScrollArea::vertical().id_salt(("strip-name",strip.id)).max_height(STRIP_NAME_HEIGHT).min_scrolled_height(STRIP_NAME_HEIGHT).auto_shrink([false,false]).show(ui,|ui|{
                let label=ui.add(egui::Label::new(RichText::new(&strip.name).strong()).wrap().sense(egui::Sense::click_and_drag()));
                self.strip_drag_handle(&label,strip);
                if label.clicked(){self.selected=Some(strip.id);}
                self.strip_context_menu(&label,strip);
            });
            self.strip_context_menu(&background,strip);
            });
        }).response.rect
    }
    fn inspector(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let Some(strip) = self.selected.and_then(|id| snapshot.scene.strip(id)) else {
            ui.label(
                RichText::new("Select a strip to edit its name, inserts, and appearance.")
                    .color(DIM),
            );
            return;
        };
        ui.heading(&strip.name);
        let name = self
            .names
            .entry(strip.id)
            .or_insert_with(|| strip.name.clone());
        let r = ui.text_edit_singleline(name);
        let rename = if r.lost_focus() && *name != strip.name && !name.trim().is_empty() {
            Some(name.clone())
        } else {
            None
        };
        if let Some(name) = rename {
            self.edit(Edit::Rename {
                strip: strip.id,
                name,
            });
        }
        if strip.kind == StripKind::Output {
            ui.label(
                RichText::new("Output names are shared with other audio applications.")
                    .small()
                    .color(DIM),
            );
            if strip.output_name_override.is_some() && ui.button("Reset name").clicked() {
                self.edit(Edit::ResetOutputName { strip: strip.id });
                if let Some(original) = &strip.output_original_name {
                    self.names.insert(strip.id, original.clone());
                }
            }
        }
        let mut color = strip.color;
        if ui.color_edit_button_srgb(&mut color).changed() {
            self.edit(Edit::Color {
                strip: strip.id,
                color,
            });
        }
        ui.horizontal(|ui| {
            for (left, label) in [(true, "←"), (false, "→")] {
                if ui.button(label).clicked() {
                    let mut order: Vec<_> = snapshot.scene.strips.iter().map(|s| s.id).collect();
                    let i = order.iter().position(|id| *id == strip.id).unwrap();
                    let j = if left {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(order.len() - 1)
                    };
                    order.swap(i, j);
                    self.edit(Edit::Reorder { strips: order });
                }
            }
        });
        if strip.kind == StripKind::SystemSounds {
            ui.separator();
            ui.label("Shared volume and mute for tagged event sounds. Each sound keeps its requested output.");
            return;
        }
        ui.separator();
        ui.label(RichText::new("INSERT EFFECTS").small().color(MINT));
        if self.selected_insert.is_some_and(|(s, _)| s == strip.id)
            && ui.small_button("Show all inserts").clicked()
        {
            self.selected_insert = None;
        }
        let active_insert = self
            .selected_insert
            .filter(|(s, id)| *s == strip.id && strip.inserts.iter().any(|i| i.id == *id))
            .map(|(_, id)| id);
        for insert in strip
            .inserts
            .iter()
            .filter(|i| active_insert.is_none_or(|id| id == i.id))
        {
            ui.group(|ui| {
                ui.label(self.plugin_name(&insert.uri));
                let mut bypass = insert.bypass;
                if ui.checkbox(&mut bypass, "Bypass").changed() {
                    self.edit(Edit::BypassInsert {
                        strip: strip.id,
                        insert: insert.id,
                        bypass,
                    });
                }
                if ui.button("Open editor…").clicked() {
                    self.selected_insert = Some((strip.id, insert.id));
                    self.ensure_catalog();
                }
                ui.horizontal(|ui| {
                    if ui.small_button("↑").clicked() {
                        let mut ids: Vec<_> = strip.inserts.iter().map(|i| i.id).collect();
                        let index = ids.iter().position(|id| *id == insert.id).unwrap();
                        if index > 0 {
                            ids.swap(index, index - 1);
                            self.edit(Edit::ReorderInserts {
                                strip: strip.id,
                                inserts: ids,
                            });
                        }
                    }
                    if ui.small_button("Remove insert").clicked() {
                        self.edit(Edit::RemoveInsert {
                            strip: strip.id,
                            insert: insert.id,
                        });
                    }
                });
            });
        }
        if ui.button("Add effect…").clicked() {
            self.open_plugin_picker(strip.id);
        }
        if let Some(identity) = &strip.identity {
            ui.separator();
            ui.label(RichText::new("STREAM IDENTITY").small().color(DIM));
            ui.label(&identity.application);
            ui.label(&identity.name);
            if let Some(stream) = snapshot
                .observation
                .streams
                .iter()
                .find(|s| s.managed == Some(strip.id))
            {
                ui.label(format!(
                    "Application input: {:.0}%{}",
                    stream.volume * 100.0,
                    if stream.mute {
                        " · muted externally"
                    } else {
                        ""
                    }
                ));
            }
        }
        if let Some(key) = &strip.output_key {
            ui.separator();
            ui.label(RichText::new("OUTPUT ENDPOINT").small().color(DIM));
            ui.label(key);
            ui.label("Device volume and mute · meters before device controls");
        }
        if matches!(strip.kind, StripKind::Group | StripKind::Aux) {
            ui.separator();
            if ui
                .button(
                    RichText::new(if strip.kind == StripKind::Aux {
                        "Delete aux master"
                    } else {
                        "Remove bus"
                    })
                    .color(RED),
                )
                .clicked()
            {
                self.edit(Edit::RemoveStrip { strip: strip.id });
                self.selected = None;
            }
        }
    }
}
impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        match self.preferences.update(ctx.zoom_factor()) {
            Ok(true) => ctx.request_repaint_after(Duration::from_millis(250)),
            Err(e) => self.error = format!("Could not save UI scale: {e}"),
            _ => {}
        }
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Snapshot(snapshot) => {
                    if snapshot.scene.strips.iter().any(|s| !s.inserts.is_empty()) {
                        self.ensure_catalog();
                    }
                    self.snapshot = Some(snapshot);
                }
                Event::Error(e) => self.error = e,
                Event::Catalog(result) => {
                    self.catalog_loading = false;
                    match result {
                        Ok(value) => {
                            self.catalog = value;
                            self.catalog_error.clear();
                        }
                        Err(e) => self.catalog_error = e,
                    }
                }
            }
        }
        let snapshot = self.snapshot.clone();
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(16.0))
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ALIXER").size(24.0).strong().color(MINT));
                    ui.label(RichText::new("A LINUX MIXER").small().color(DIM));
                    ui.separator();
                    ui.menu_button("Options", |ui| {
                        if ui.button("Reset channel colors").clicked() {
                            self.edit(Edit::ResetChannelColors);
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("+ Aux").clicked() {
                            self.new_bus = Some(true);
                            self.bus_name = "Aux".into();
                        }
                        if ui.button("+ Group").clicked() {
                            self.new_bus = Some(false);
                            self.bus_name = "Group".into();
                        }
                    });
                });
            });
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        snapshot
                            .as_ref()
                            .map(|s| s.status.as_str())
                            .unwrap_or("Waiting for service"),
                    )
                    .small()
                    .color(DIM),
                );
                if let Some(s) = &snapshot {
                    ui.label(
                        RichText::new(if s.conflicts.is_empty() {
                            format!("Revision {}", s.scene.revision)
                        } else {
                            format!("Revision {} · {} need attention", s.scene.revision, s.conflicts.len())
                        })
                        .small()
                        .color(DIM),
                    );
                }
            });
            if snapshot.as_ref().is_some_and(|s| {
                s.scene.strips.iter().any(|strip| {
                    strip.kind == StripKind::Output
                        && (strip.output_name_override.is_some()
                            || strip.output_original_name.as_ref() == Some(&strip.name))
                        && s.observation.outputs.iter().any(|o| {
                            strip.output_key.as_ref() == Some(&o.key) && strip.name != o.name
                        })
                })
            }) {
                ui.horizontal(|ui| {
                    ui.label("Output name changes need a WirePlumber restart.");
                    if ui.button("Apply output names…").clicked() {
                        self.confirm_output_names = true;
                    }
                });
            }
            if !self.error.is_empty() {
                ui.colored_label(RED, &self.error);
            }
        });
        if self.confirm_output_names {
            egui::Window::new("Apply output names")
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(
                        "Restart WirePlumber to apply output names in all audio applications.",
                    );
                    ui.label("Audio may briefly stop while devices reconnect.");
                    ui.horizontal(|ui| {
                        if ui.button("Restart WirePlumber").clicked() {
                            self.send(Command::ApplyOutputNames);
                            self.confirm_output_names = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_output_names = false;
                        }
                    });
                });
        }
        if let Some(snapshot) = &snapshot {
            if self.selected.is_some() {
                egui::Panel::right("inspector")
                    .default_size(270.0)
                    .show(root, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui, snapshot));
                    });
            }
            egui::CentralPanel::default().show(root, |ui| self.console(ui, snapshot));
        } else {
            egui::CentralPanel::default().show(root, |ui| {
                ui.add_space(100.0);
                ui.vertical_centered(|ui| {
                    ui.heading("The console is ready.");
                    ui.label("Start alixerd or enable alixer.service to connect your audio.");
                    ui.label(
                        RichText::new("The GUI can close independently of the mixer engine.")
                            .color(DIM),
                    );
                });
            });
        }
        if let Some(snapshot) = &snapshot {
            self.plugin_picker_window(&ctx, snapshot);
            self.effect_editor_window(&ctx, snapshot);
            self.rename_strip_window(&ctx, snapshot);
        }
        if let Some(aux) = self.new_bus {
            egui::Window::new(if aux { "New aux bus" } else { "New subgroup" })
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.text_edit_singleline(&mut self.bus_name);
                    ui.horizontal(|ui| {
                        if ui.button("Create stereo").clicked() && !self.bus_name.trim().is_empty()
                        {
                            self.edit(Edit::AddBus {
                                name: self.bus_name.clone(),
                                aux,
                                channels: vec!["FL".into(), "FR".into()],
                            });
                            self.new_bus = None;
                        }
                        if ui.button("Create mono").clicked() && !self.bus_name.trim().is_empty() {
                            self.edit(Edit::AddBus {
                                name: self.bus_name.clone(),
                                aux,
                                channels: vec!["MONO".into()],
                            });
                            self.new_bus = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.new_bus = None;
                        }
                    });
                });
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut dev_service = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--dev-service" => dev_service = true,
            "--help" | "-h" => {
                println!(
                    "Usage: alixer [--dev-service]\n\n  --dev-service  Spawn alixerd for this GUI session and stop it on exit.\n                 Build both binaries first with cargo build --workspace."
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown option {arg}; see --help").into()),
        }
    }
    migrate_legacy_state()?;
    let _owned_service = if dev_service {
        Some(dev_service::OwnedService::start()?)
    } else {
        None
    };
    eframe::run_native(
        "Alixer - A Linux Mixer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_app_id("org.alixer.Alixer")
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([900.0, 600.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
    .map_err(Into::into)
}

#[cfg(test)]
fn test_app() -> (App, mpsc::Receiver<Command>) {
    let (tx, commands) = mpsc::channel();
    let (_events, rx) = mpsc::channel();
    let app = App {
        tx,
        rx,
        snapshot: None,
        error: String::new(),
        selected: None,
        new_bus: None,
        bus_name: String::new(),
        catalog: serde_json::Value::Null,
        catalog_requested: false,
        catalog_loading: false,
        catalog_error: String::new(),
        plugin_picker: None,
        selected_insert: None,
        rename_strip: None,
        confirm_output_names: false,
        filter: String::new(),
        show_inactive: false,
        strip_presence: BTreeMap::new(),
        names: BTreeMap::new(),
        preferences: preferences::Preferences::load(
            std::env::temp_dir().join("alixer-layout-unused.json"),
        ),
    };
    (app, commands)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn meter_levels_align_with_fader_handle_and_db_labels() {
        let (mut app, _) = test_app();
        let strip = Strip::new("Music", StripKind::Stream, vec!["FL".into(), "FR".into()]);
        for peak in [0.5_f32, 1.0, 2.0] {
            let snapshot = Snapshot {
                active_streams: None,
                scene: Scene {
                    strips: vec![strip.clone()],
                    ..Default::default()
                },
                observation: Observation::default(),
                conflicts: vec![],
                status: String::new(),
                peaks: vec![(strip.id, peak)],
                channel_peaks: vec![],
                plugin_meters: vec![],
            };
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.strip(ui, &strip, &snapshot, STRIP_MIN_HEIGHT);
            });
            let label_y = |label: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::epaint::Shape::Text(t) if t.galley.text() == label => {
                            Some(t.pos.y + t.galley.size().y / 2.0)
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            let zero = label_y("0");
            let minus12 = label_y("−12");
            let bar = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Rect(r)
                        if (r.fill == MINT || r.fill == RED)
                            && (r.rect.width() - 14.0).abs() < 0.1 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .unwrap();
            let knob = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Rect(r)
                        if (r.rect.width() - 18.0).abs() < 0.1
                            && (r.rect.height() - 12.0).abs() < 0.1 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .unwrap();
            assert!((knob.center().y - zero).abs() < 0.1);
            let db = 20.0 * peak.log10();
            let expected = zero - (minus12 - zero) * db / 12.0;
            assert!(
                (bar.top() - expected).abs() < 0.1,
                "Meter at {db} dBFS must align with the scale"
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn strips_stay_narrow_in_a_horizontal_bank() {
        let (mut app, _commands) = test_app();
        app.show_inactive = true;
        for channels in [0, 2, 8, 64] {
            let mut strip = if channels == 0 {
                Strip::new("Spotify", StripKind::Stream, vec!["FL".into(), "FR".into()])
            } else {
                Strip::new(
                    "Speakers",
                    StripKind::Output,
                    (0..channels).map(|ch| format!("AUX{ch}")).collect(),
                )
            };
            if channels > 0 {
                strip.output_key = Some("speakers".into());
            }
            let mut scene = Scene::default();
            for n in 0..12 {
                let mut target = Strip::new(
                    format!("Destination {n}"),
                    StripKind::Output,
                    (0..8).map(|ch| format!("AUX{ch}")).collect(),
                );
                target.output_key = Some(format!("layout-output-{n}"));
                scene.strips.push(target);
            }
            let snapshot = Snapshot {
                active_streams: None,
                scene,
                observation: Observation::default(),
                conflicts: vec![],
                status: String::new(),
                peaks: vec![(strip.id, 0.5)],
                channel_peaks: vec![],
                plugin_meters: vec![],
            };
            for width in [900.0, 2048.0] {
                for height in [STRIP_MIN_HEIGHT, 720.0, 1000.0] {
                    let ctx = egui::Context::default();
                    ctx.style_mut_of(egui::Theme::Dark, |s| {
                        s.spacing.item_spacing = Vec2::new(9.0, 9.0)
                    });
                    let mut long = strip.clone();
                    long.id = Id::new_v4();
                    long.name = "USB Audio speakers with a very long device name ".repeat(100);
                    long.inserts = (0..40)
                        .map(|_| Insert {
                            id: Id::new_v4(),
                            uri: format!("urn:test:{}", "very-long-plugin-name".repeat(20)),
                            bypass: false,
                            controls: BTreeMap::new(),
                            state: None,
                        })
                        .collect();
                    app.selected = Some(strip.id);
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(width, height + 40.0),
                        )),
                        ..Default::default()
                    };
                    for _ in 0..5 {
                        let mut rects = None;
                        let mut output = ctx.run_ui(input.clone(), |ui| {
                    ui.horizontal_top(|ui| {
                        let first = app.strip(ui, &strip, &snapshot,height);
                        let second = app.strip(ui, &long, &snapshot,height);
                        assert!(
                            (first.width()-162.0).abs()<0.1&&(second.width()-162.0).abs()<0.1,
                            "Strip expanded to {} points",
                            first.width()
                        );
                        assert!(first.height() > 300.0, "Controls should stack vertically");
                        assert!((first.height()-height).abs()<1.0,"Expected {height}, got {}",first.height());
                        assert!((second.height()-first.height()).abs()<1.0,"Long names and multichannel layouts must not change strip height: {} vs {}",first.height(),second.height());
                        assert!(second.left() >= first.right(), "Adjacent strips overlap");
                        rects=Some((first,second));
                    });
                });
                        let (first, second) = rects.unwrap();
                        for (name, rect) in [(&strip.name, first), (&long.name, second)] {
                            let text = output
                                .shapes
                                .iter()
                                .find_map(|s| match &s.shape {
                                    egui::epaint::Shape::Text(text)
                                        if text.galley.text() == name =>
                                    {
                                        Some((text, s.clip_rect))
                                    }
                                    _ => None,
                                })
                                .expect("strip name must be drawn");
                            assert!(
                                text.0.pos.y >= rect.bottom() - STRIP_NAME_HEIGHT - 16.0,
                                "Name must be at the bottom of its strip"
                            );
                            assert!(
                                text.1.height() <= STRIP_NAME_HEIGHT + 2.0,
                                "Name must be clipped to its fixed-height scroll area"
                            );
                        }
                        output.textures_delta.clear();
                    }
                }
            }
        }
    }
}
