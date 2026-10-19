//! [`Panel`]: what a device's panel draws with. It reads the device's
//! parameter values, meters and tapped sound, and turns knob moves into
//! live edits that end as one undo step per gesture.

use effects::MAX_METERS;
use eframe::egui::{self, Color32, Response, RichText, Sense, Ui};
use instruments::{EnvelopeParams, ParamDescriptor, ParameterKind};
use project::{AutomationTarget, Device, DeviceId, Track, TrackId, TrackKind};

use crate::session::Session;
use crate::taps::Tap;
use crate::theme::{size, space, text};
use crate::timeline::{Action, TimelineView, clicked_control_menu_with};
use crate::widgets::keys::KeyEvent;
use crate::widgets::{EnvelopeEditor, Knob};
use crate::widgets::{knob, value_entry};

/// The height a knob's name and value add to its diameter, with the gap
/// under a row of them.
pub(crate) const KNOB_TEXT: f32 = knob::LABEL_HEIGHT + knob::VALUE_HEIGHT + space::S - 1.0;

/// Where a display's name (or other corner note) sits, from its corner.
pub(crate) const DISPLAY_INSET: egui::Vec2 = egui::vec2(space::S + space::XS, space::S);

/// What a row of switches or menus takes of a panel's height, with the
/// gaps around it: for panels sizing a display to the height left.
pub(crate) const CONTROL_ROW: f32 = size::CONTROL + 2.0 * space::M;

/// Puts a control (a menu, a few buttons) in a row of knobs `diameter`
/// across: `label` (if any) where a knob's name is, and the control across
/// the middle of where a knob's circle is, so the row lines up.
pub(crate) fn knob_slot<R>(
    ui: &mut Ui,
    diameter: f32,
    label: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let top = ui.cursor().top();
        if !label.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(label)
                        .size(text::SMALL)
                        .color(ui.visuals().text_color()),
                )
                .selectable(false),
            );
        }
        let middle = top + knob::LABEL_HEIGHT + diameter / 2.0;
        ui.add_space((middle - size::CONTROL / 2.0 - ui.cursor().top()).max(0.0));
        add(ui)
    })
    .inner
}

/// Whose parameters a panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    Device(DeviceId),
    Instrument(TrackId),
}

pub(crate) struct Panel<'a> {
    pub session: &'a mut Session,
    pub timeline: &'a mut TimelineView,
    /// The track the device is on; `None` for the master.
    pub track: Option<TrackId>,
    pub owner: Owner,
    /// The effect's or instrument's id, like "canopy.eq".
    pub kind: &'static str,
    /// Built-in or a plugin (whose panel is the plugin host's).
    pub format: effects::Format,
    pub params: &'static [ParamDescriptor],
    pub settings: &'static [ParamDescriptor],
    /// An instrument's envelopes, drawn as curves rather than knobs.
    pub envelopes: &'static [EnvelopeParams],
    values: Vec<f32>,
    setting_values: Vec<f32>,
    automated: Vec<bool>,
    /// The track's color: automated parameters are named in it.
    pub color: Color32,
    /// The device's meters, as its descriptor lists them.
    pub meters: [f32; MAX_METERS],
    pub knob_size: f32,
    pub sample_rate: f32,
}

impl<'a> Panel<'a> {
    /// A panel for an effect, or `None` if this build doesn't have it.
    pub fn effect(
        session: &'a mut Session,
        timeline: &'a mut TimelineView,
        track: Option<TrackId>,
        device: &Device,
        color: Color32,
    ) -> Option<Panel<'a>> {
        let descriptor = device.effect.descriptor()?;
        let lanes = track.and_then(|t| session.project.track(t));
        let automated = descriptor
            .params
            .iter()
            .map(|p| {
                let target = AutomationTarget::DeviceParam {
                    device: device.id,
                    param: p.id.to_owned(),
                };
                lanes.is_some_and(|t| t.automation_lane(&target).is_some())
            })
            .collect();
        let setting_values = descriptor
            .settings
            .iter()
            .map(|s| device.setting(s.id).unwrap_or(s.default_value()))
            .collect();
        Some(Panel {
            meters: session.device_meters(device.id),
            sample_rate: session.sample_rate() as f32,
            session,
            timeline,
            track,
            owner: Owner::Device(device.id),
            kind: descriptor.id,
            format: device.effect.format,
            params: descriptor.params,
            settings: descriptor.settings,
            envelopes: &[],
            values: device.values(),
            setting_values,
            automated,
            color,
            knob_size: 34.0,
        })
    }

    /// A panel for a track's instrument, or `None` if it has none this
    /// build knows.
    pub fn instrument(
        session: &'a mut Session,
        timeline: &'a mut TimelineView,
        track: &Track,
        color: Color32,
    ) -> Option<Panel<'a>> {
        let TrackKind::Midi { instrument, .. } = &track.kind else {
            return None;
        };
        let descriptor = instrument.descriptor()?;
        let values = track.instrument_values();
        // From the lanes rather than a lookup per parameter: a drum kit has
        // thousands of parameters, and this runs every frame.
        let mut automated = vec![false; descriptor.params.len()];
        for lane in &track.automation {
            if let AutomationTarget::InstrumentParam(id) = &lane.target
                && let Some(index) = descriptor.params.iter().position(|p| p.id == id.as_str())
            {
                automated[index] = true;
            }
        }
        Some(Panel {
            meters: session.instrument_meters(track.id),
            sample_rate: session.sample_rate() as f32,
            session,
            timeline,
            track: Some(track.id),
            owner: Owner::Instrument(track.id),
            kind: descriptor.id,
            format: instrument.format,
            params: descriptor.params,
            settings: &[],
            envelopes: descriptor.envelopes,
            values,
            setting_values: Vec::new(),
            automated,
            color,
            knob_size: 34.0,
        })
    }

    /// Sizes knobs so `rows` rows of them fill the height left (within
    /// reason), and returns the height of a row.
    pub fn fit_knobs(&mut self, ui: &Ui, rows: usize) -> f32 {
        self.fit_knobs_beside(ui, rows, 0.0)
    }

    /// Like [`Panel::fit_knobs`], leaving `other` points for other rows.
    pub fn fit_knobs_beside(&mut self, ui: &Ui, rows: usize, other: f32) -> f32 {
        let row = (ui.available_height() - other) / rows.max(1) as f32;
        self.knob_size = (row - KNOB_TEXT).clamp(26.0, 40.0);
        self.knob_size + KNOB_TEXT
    }

    /// An id for a widget of this panel's, unique per device.
    pub fn id(&self, salt: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
        egui::Id::new(("panel", self.owner, salt))
    }

    pub fn index(&self, id: &str) -> Option<usize> {
        self.params.iter().position(|p| p.id == id)
    }

    /// Parameter `id`'s value, or 0 if there's no such parameter.
    pub fn value(&self, id: &str) -> f32 {
        self.index(id).map_or(0.0, |i| self.values[i])
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Changes a parameter as part of the current gesture: the engine hears
    /// it at once; [`Panel::finish`] makes the gesture an undo step.
    pub fn set(&mut self, index: usize, value: f32) {
        let Some(param) = self.params.get(index) else {
            return;
        };
        let value = param.clamp(value);
        match self.owner {
            Owner::Device(device) => self
                .session
                .live_device_param(device, index, param.id, value),
            Owner::Instrument(track) => self.session.live_param(track, index, param.id, value),
        }
        self.values[index] = value;
    }

    pub fn set_id(&mut self, id: &str, value: f32) {
        if let Some(index) = self.index(id) {
            self.set(index, value);
        }
    }

    /// Ends the current gesture as one undo step.
    pub fn finish(&mut self) {
        let label = match self.owner {
            Owner::Device(_) => "Change Effect",
            Owner::Instrument(_) => "Change Parameter",
        };
        self.session.end_live(label);
    }

    fn target(&self, param: &ParamDescriptor) -> AutomationTarget {
        match self.owner {
            Owner::Device(device) => AutomationTarget::DeviceParam {
                device,
                param: param.id.to_owned(),
            },
            Owner::Instrument(_) => AutomationTarget::InstrumentParam(param.id.to_owned()),
        }
    }

    /// The parameter's name color: the track's when it's automated.
    pub fn name_color(&self, index: usize) -> Option<Color32> {
        self.automated
            .get(index)
            .copied()
            .unwrap_or(false)
            .then_some(self.color)
    }

    /// What every parameter control does besides changing the value: the
    /// right-click menu (type a value, reset, automate, link a controller),
    /// double-click to type a value, Ctrl+click (Cmd on a Mac) to reset,
    /// and ending the gesture.
    pub fn attach(&mut self, response: &Response, index: usize) {
        self.attach_with(response, index, true);
    }

    /// [`Panel::attach`]; `value` is false for choices (buttons, menus),
    /// which have nothing to type and nothing to reset.
    fn attach_with(&mut self, response: &Response, index: usize, value: bool) {
        let param = self.params[index];
        // Under its own popup id: a menu button's own popup has the
        // button's, and sharing it would close the menu as it opens.
        let mut menu = response.clone();
        menu.id = response.id.with("control menu");
        let (mut type_in, mut reset) = (false, false);
        clicked_control_menu_with(
            self.timeline,
            &menu,
            self.track,
            self.target(&param),
            |ui| {
                if !value {
                    return;
                }
                type_in = ui.button("Type Value…").clicked();
                reset = ui.button("Reset to Default").clicked();
                if type_in || reset {
                    ui.close();
                }
                ui.separator();
            },
        );
        let ended = response.drag_stopped() || (response.changed() && !response.dragged());
        if !value {
            if ended {
                self.finish();
            }
            return;
        }
        let command_click =
            response.clicked() && response.ctx.input(|i| i.modifiers.command_only());
        if reset || command_click {
            self.set(index, param.default_value());
            self.session.end_live("Reset Parameter");
            return;
        }
        if type_in {
            value_entry::open(response, &param, self.values[index]);
        }
        if let Some(typed) = value_entry::show(response, &param, self.values[index]) {
            self.set(index, typed);
            self.finish();
        } else if ended {
            self.finish();
        }
    }

    /// A knob for parameter `id`, named as the parameter is.
    pub fn knob(&mut self, ui: &mut Ui, id: &str) -> Option<Response> {
        self.knob_as(ui, id, None)
    }

    /// A knob for parameter `id` with a shorter name.
    pub fn knob_as(&mut self, ui: &mut Ui, id: &str, label: Option<&str>) -> Option<Response> {
        self.knob_with(ui, id, label, None)
    }

    /// A knob that also shows where modulation has the value right now
    /// (`live`, in the parameter's units).
    pub fn knob_with(
        &mut self,
        ui: &mut Ui,
        id: &str,
        label: Option<&str>,
        live: Option<f32>,
    ) -> Option<Response> {
        let index = self.index(id)?;
        Some(self.knob_at(ui, index, label, live))
    }

    /// A knob for parameter `index` (see [`Panel::knob_with`]), for panels
    /// that know where their parameters are.
    pub fn knob_at(
        &mut self,
        ui: &mut Ui,
        index: usize,
        label: Option<&str>,
        live: Option<f32>,
    ) -> Response {
        let param = &self.params[index];
        let mut knob = Knob::new(self.id(("knob", index)), param, self.values[index])
            .diameter(self.knob_size)
            .label_color(self.name_color(index))
            .modulated(live.map(|v| param.to_normalized(v)));
        if let Some(label) = label {
            knob = knob.label(label);
        }
        let out = knob.show(ui);
        if let Some(value) = out.changed {
            self.set(index, value);
        }
        self.attach(&out.response, index);
        out.response
    }

    /// Knobs for several parameters in a row (continuing the row `ui` is
    /// already laying out, if it is one).
    pub fn knobs(&mut self, ui: &mut Ui, ids: &[&str]) {
        if ui.layout().main_dir().is_horizontal() {
            for id in ids {
                self.knob(ui, id);
            }
            return;
        }
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for id in ids {
                self.knob(ui, id);
            }
        });
    }

    /// A choice: buttons side by side for a few options, a menu for more.
    pub fn choice(&mut self, ui: &mut Ui, id: &str) -> Option<Response> {
        let index = self.index(id)?;
        Some(self.choice_at(ui, index))
    }

    /// [`Panel::choice`] for parameter `index`.
    pub fn choice_at(&mut self, ui: &mut Ui, index: usize) -> Response {
        let ParameterKind::Enum { labels, .. } = self.params[index].kind else {
            return self.knob_at(ui, index, None, None);
        };
        let current = self.values[index] as usize;
        if labels.len() > 4 {
            let mut chosen = None;
            let response = self.menu(ui, index, labels, current, &mut chosen);
            return self.chose(response, index, chosen);
        }
        // Each button carries the right-click menu itself.
        let row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for (i, label) in labels.iter().enumerate() {
                let response = ui.selectable_label(current == i, *label);
                let chosen = response.clicked().then_some(i);
                self.chose(response, index, chosen);
            }
        });
        row.response
    }

    /// A choice shown as a menu button, however few its options.
    pub fn choice_menu(&mut self, ui: &mut Ui, id: &str) -> Option<Response> {
        let index = self.index(id)?;
        self.choice_menu_at(ui, index)
    }

    /// [`Panel::choice_menu`] for parameter `index`.
    pub fn choice_menu_at(&mut self, ui: &mut Ui, index: usize) -> Option<Response> {
        let ParameterKind::Enum { labels, .. } = self.params[index].kind else {
            return None;
        };
        let current = self.values[index] as usize;
        let mut chosen = None;
        let response = self.menu(ui, index, labels, current, &mut chosen);
        Some(self.chose(response, index, chosen))
    }

    fn menu(
        &self,
        ui: &mut Ui,
        index: usize,
        labels: &[&str],
        current: usize,
        chosen: &mut Option<usize>,
    ) -> Response {
        let name = labels.get(current).copied().unwrap_or("?");
        let text = match self.name_color(index) {
            Some(color) => RichText::new(name).color(color),
            None => RichText::new(name),
        };
        ui.menu_button(text, |ui| {
            for (i, label) in labels.iter().enumerate() {
                if ui.selectable_label(current == i, *label).clicked() {
                    *chosen = Some(i);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(self.params[index].name)
    }

    /// Applies a choice made on `response` (a button that senses clicks).
    fn chose(&mut self, mut response: Response, index: usize, chosen: Option<usize>) -> Response {
        if let Some(i) = chosen {
            self.set(index, i as f32);
            response.mark_changed();
        }
        self.attach_with(&response, index, false);
        response
    }

    /// A switch for an Off/On parameter, labelled `label`.
    pub fn toggle(&mut self, ui: &mut Ui, id: &str, label: &str) -> Option<Response> {
        let index = self.index(id)?;
        Some(self.toggle_at(ui, index, label))
    }

    /// [`Panel::toggle`] for parameter `index`.
    pub fn toggle_at(&mut self, ui: &mut Ui, index: usize, label: &str) -> Response {
        let on = self.values[index] >= 0.5;
        let text = match self.name_color(index) {
            Some(color) if !on => RichText::new(label).color(color),
            _ => RichText::new(label),
        };
        let mut response = ui
            .selectable_label(on, text)
            .on_hover_text(self.params[index].name);
        if response.clicked() {
            self.set(index, if on { 0.0 } else { 1.0 });
            response.mark_changed();
        }
        self.attach_with(&response, index, false);
        response
    }

    /// Instrument envelope `n` as an editable curve, `size` big.
    pub fn envelope(&mut self, ui: &mut Ui, n: usize, size: egui::Vec2) {
        let Some(envelope) = self.envelopes.get(n).copied() else {
            return;
        };
        let values = self.values.clone();
        let value = |index: usize| values.get(index).copied().unwrap_or(0.0);
        let id = match self.owner {
            Owner::Instrument(track) => egui::Id::new(("envelope", track.0, n)),
            Owner::Device(_) => self.id(("envelope", n)),
        };
        let editor = EnvelopeEditor::new(id, envelope, self.params, &value)
            .color(self.color)
            .size(size);
        let response = editor.show(ui);
        for (index, new_value) in response.changes {
            self.set(index, new_value);
        }
        if response.finished {
            self.session.end_live("Change Envelope");
        }
        if let Some(editor) = &response.response
            && let Some(track) = self.track
        {
            editor.context_menu(|ui| {
                for index in envelope.indices() {
                    let param = &self.params[index];
                    if ui.button(format!("Automate {}", param.name)).clicked() {
                        let target = AutomationTarget::InstrumentParam(param.id.to_owned());
                        self.timeline.push(Action::ShowAutomation(track, target));
                        ui.close();
                    }
                }
            });
        }
    }

    /// Whether parameter `index` is part of one of the instrument's
    /// envelopes (and so drawn by [`Panel::envelope`]).
    pub fn in_envelope(&self, index: usize) -> bool {
        self.envelopes.iter().any(|e| e.contains(index))
    }

    /// UI setting `id`'s value (an analyzer's range), or 0 if there's none.
    pub fn setting(&self, id: &str) -> f32 {
        self.settings
            .iter()
            .position(|s| s.id == id)
            .map_or(0.0, |i| self.setting_values[i])
    }

    /// Changes a UI setting, as its own undo step.
    pub fn set_setting(&mut self, id: &str, value: f32) {
        let Owner::Device(device) = self.owner else {
            return;
        };
        let Some(index) = self.settings.iter().position(|s| s.id == id) else {
            return;
        };
        let value = self.settings[index].clamp(value);
        if self.setting_values[index] == value {
            return;
        }
        self.setting_values[index] = value;
        self.session
            .edit("Change View", |p| p.set_device_param(device, id, value));
    }

    pub fn setting_on(&self, id: &str) -> bool {
        self.setting(id) >= 0.5
    }

    /// The settings menu items for every setting: switches, choices, and
    /// sliders, as a menu's contents.
    pub fn settings_menu(&mut self, ui: &mut Ui) {
        for setting in self.settings {
            let value = self.setting(setting.id);
            match setting.kind {
                ParameterKind::Enum { labels, .. } if labels == ["Off", "On"] => {
                    let mut on = value >= 0.5;
                    if ui.checkbox(&mut on, setting.name).changed() {
                        self.set_setting(setting.id, if on { 1.0 } else { 0.0 });
                    }
                }
                ParameterKind::Enum { labels, .. } => {
                    ui.menu_button(setting.name, |ui| {
                        for (i, label) in labels.iter().enumerate() {
                            if ui.selectable_label(value as usize == i, *label).clicked() {
                                self.set_setting(setting.id, i as f32);
                            }
                        }
                    });
                }
                ParameterKind::Float {
                    min,
                    max,
                    logarithmic,
                    ..
                } => {
                    ui.horizontal(|ui| {
                        ui.label(setting.name);
                        let mut v = value;
                        let response = ui.add(
                            egui::Slider::new(&mut v, min..=max)
                                .logarithmic(logarithmic)
                                .custom_formatter(|v, _| setting.format(v as f32)),
                        );
                        if response.drag_stopped() || (response.changed() && !response.dragged()) {
                            self.set_setting(setting.id, v);
                        } else if response.changed() {
                            // Shown while dragging; stored when it ends.
                            if let Some(i) = self.settings.iter().position(|s| s.id == setting.id) {
                                self.setting_values[i] = v;
                            }
                        }
                    });
                }
            }
        }
    }

    /// Plays or lets go of a note on the instrument (a key or pad clicked
    /// in its panel).
    pub fn play(&mut self, event: KeyEvent) {
        let Owner::Instrument(track) = self.owner else {
            return;
        };
        let (key, down) = match event {
            KeyEvent::Down(key) => (key, true),
            KeyEvent::Up(key) => (key, false),
        };
        let Some(pitch) = midi::Pitch::new(key) else {
            return;
        };
        let kind = if down {
            midi::NoteEventKind::On {
                pitch,
                velocity: midi::Velocity::from_midi1(100),
            }
        } else {
            midi::NoteEventKind::Off {
                pitch,
                velocity: midi::Velocity::CENTER,
            }
        };
        self.session.preview_note(track, kind);
    }

    /// Whether this effect has a track routed to its sidechain.
    pub fn session_has_sidechain(&self) -> bool {
        match self.owner {
            Owner::Device(device) => self
                .session
                .project
                .device(device)
                .is_some_and(|d| d.sidechain.is_some()),
            Owner::Instrument(_) => false,
        }
    }

    /// The device's recent sound, if it taps it.
    pub fn tap(&self) -> Option<&Tap> {
        match self.owner {
            Owner::Device(device) => self.session.tap(device),
            Owner::Instrument(_) => None,
        }
    }

    /// Keeps frames coming while the device's readings move. Levels rest at
    /// a floor rather than at zero, so what counts is change since the last
    /// frame.
    ///
    /// For a little while after anything happens (the readings move, a key
    /// or the mouse, MIDI coming in), it also checks now and then, slowly,
    /// for sound that starts without anything else repainting (a note's
    /// tail reaching an effect). Then it lets the window sleep: an idle
    /// window doesn't repaint.
    pub fn animate(&self, ui: &Ui) {
        /// How long it keeps checking after the last thing that happened.
        const LISTEN: f64 = 2.0;
        let last_id = self.id("last meters");
        let last: Option<[f32; MAX_METERS]> = ui.data(|d| d.get_temp(last_id));
        let changed = last.is_none_or(|last| {
            last.iter()
                .zip(&self.meters)
                .any(|(a, b)| (a - b).abs() > 1e-3)
        });
        ui.data_mut(|d| d.insert_temp(last_id, self.meters));
        let busy = changed || self.tap().is_some_and(Tap::active) || self.session.is_playing();
        if busy {
            ui.ctx().request_repaint();
        }
        let (now, input) = ui.input(|i| (i.time, !i.events.is_empty()));
        let since_id = self.id("listening since");
        if busy || input || self.session.midi.active() {
            ui.data_mut(|d| d.insert_temp(since_id, now));
        }
        let since = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(since_id, || now));
        if !busy && now - since < LISTEN {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// The dark, framed area a device draws a display in (a curve, a scope).
pub(crate) fn display(ui: &mut Ui, size: egui::Vec2, sense: Sense) -> (Response, egui::Painter) {
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let corner = egui::CornerRadius::same(crate::theme::radius::PANEL);
    painter.rect_filled(rect, corner, visuals.extreme_bg_color);
    painter.rect_stroke(
        rect,
        corner,
        visuals.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    (response, painter)
}
