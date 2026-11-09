use crate::{
    config::{Camera, Config},
    config_save,
    discover::{self, Candidate, Interface, Plan, Scan, Subnet},
    render::{VideoCallback, VideoRenderer},
    stream::{Player, Source},
};
use eframe::egui::{self, Color32, Rect, Sense, Vec2};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

struct Draft {
    id: String,
    camera: Camera,
    username: String,
    password_env: String,
    note: String,
    checked: bool,
}

impl Draft {
    fn new(candidate: Candidate) -> Self {
        Self {
            id: config_save::identity(&candidate.camera),
            username: candidate.camera.username.clone().unwrap_or_default(),
            password_env: candidate.camera.password_env.clone().unwrap_or_default(),
            camera: candidate.camera,
            note: candidate.note,
            checked: false,
        }
    }

    fn camera(&self) -> Camera {
        let mut camera = self.camera.clone();
        camera.username =
            (!self.username.trim().is_empty()).then(|| self.username.trim().to_owned());
        camera.password_env =
            (!self.password_env.trim().is_empty()).then(|| self.password_env.trim().to_owned());
        camera
    }
}

pub struct DiscoveryUi {
    path: PathBuf,
    interfaces: Vec<Interface>,
    interface: usize,
    range: String,
    scan: Option<Scan>,
    drafts: Vec<Draft>,
    selected: Option<usize>,
    preview: Option<Player>,
    error: Option<String>,
    existing: Vec<Camera>,
}

impl DiscoveryUi {
    pub fn new(path: &Path, context: &egui::Context) -> Self {
        let (interfaces, error) = match discover::interfaces() {
            Ok(interfaces) => (interfaces, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let range = interfaces.first().map(default_range).unwrap_or_default();
        let mut view = Self {
            path: path.to_owned(),
            interfaces,
            interface: 0,
            range,
            scan: None,
            drafts: Vec::new(),
            selected: None,
            preview: None,
            error,
            existing: Config::load(path).map(|c| c.cameras).unwrap_or_default(),
        };
        if !view.interfaces.is_empty() {
            view.start(context);
        }
        view
    }

    fn start(&mut self, context: &egui::Context) {
        let result = (|| {
            let interface = self
                .interfaces
                .get(self.interface)
                .ok_or_else(|| anyhow::anyhow!("No active IPv4 network interface found"))?
                .clone();
            let plan = Plan::local(interface, Subnet::parse(&self.range)?)?;
            self.start_plan(plan, context)
        })();
        if let Err(error) = result {
            self.error = Some(error.to_string());
        }
    }

    fn start_plan(&mut self, plan: Plan, context: &egui::Context) -> anyhow::Result<()> {
        self.scan = None;
        self.preview = None;
        self.drafts.clear();
        self.selected = None;
        self.error = None;
        let context = context.clone();
        self.scan = Some(Scan::start(plan, move || context.request_repaint())?);
        Ok(())
    }

    fn poll(&mut self) {
        let Some(scan) = &self.scan else { return };
        let mut known: HashSet<_> = self.drafts.iter().map(|d| d.id.clone()).collect();
        for mut candidate in scan.snapshot().found {
            let id = config_save::identity(&candidate.camera);
            if known.insert(id.clone()) {
                if let Some(existing) = self
                    .existing
                    .iter()
                    .find(|c| config_save::identity(c) == id)
                {
                    candidate.camera = existing.clone();
                    candidate.note = "Already configured".into();
                }
                self.drafts.push(Draft::new(candidate));
            }
        }
        if self.selected.is_none() && !self.drafts.is_empty() {
            self.selected = Some(0);
        }
    }

    fn start_preview(&mut self, index: usize, context: &egui::Context) {
        self.selected = Some(index);
        self.preview = None;
        let camera = self.drafts[index].camera();
        match camera.endpoint() {
            Ok(_) => {
                let context = context.clone();
                self.preview = Some(Player::new(Source::Camera(camera), move || {
                    context.request_repaint()
                }));
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn save(&mut self) -> anyhow::Result<usize> {
        let cameras: Vec<_> = self
            .drafts
            .iter()
            .filter(|d| d.checked)
            .map(Draft::camera)
            .collect();
        config_save::append(&self.path, &cameras)
    }

    /// Return true after saving selected cameras, so the app can open the camera wall.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &egui_wgpu::RenderState) -> bool {
        self.poll();
        let context = ui.ctx().clone();
        let mut saved = false;
        ui.heading("Discover cameras");
        ui.horizontal_wrapped(|ui| {
            let before = self.interface;
            egui::ComboBox::from_id_salt("discovery-interface")
                .selected_text(
                    self.interfaces
                        .get(self.interface)
                        .map(|i| format!("{} · {}", i.name, i.address))
                        .unwrap_or_else(|| "No network".into()),
                )
                .show_ui(ui, |ui| {
                    for (index, interface) in self.interfaces.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.interface,
                            index,
                            format!("{} · {}", interface.name, interface.address),
                        );
                    }
                });
            if self.interface != before {
                self.range = default_range(&self.interfaces[self.interface]);
            }
            ui.label("Scan range");
            ui.add(egui::TextEdit::singleline(&mut self.range).desired_width(155.0));
            if ui.button("Rescan").clicked() {
                self.start(&context);
            }
            if let Some(scan) = &self.scan {
                let progress = scan.snapshot();
                if progress.active {
                    if ui.button("Stop scan").clicked() {
                        scan.stop();
                    }
                    ui.label(format!(
                        "Checking {} / {} ports",
                        progress.completed, progress.total
                    ));
                } else {
                    ui.label(format!("Scan finished · {} results", self.drafts.len()));
                }
            }
        });
        ui.label("ONVIF discovery and RTSP ports 554, 8554, 10554. Select a camera to preview it.");
        if let Some(scan) = &self.scan {
            for warning in scan.snapshot().warnings {
                ui.colored_label(Color32::YELLOW, warning);
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(Color32::from_rgb(245, 155, 80), error);
        }
        ui.separator();

        let selected_count = self.drafts.iter().filter(|draft| draft.checked).count();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    selected_count > 0,
                    egui::Button::new(format!("Save selected ({selected_count}) and open wall")),
                )
                .clicked()
            {
                match self.save() {
                    Ok(_) => saved = true,
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            ui.small(format!("Save to {}", self.path.display()));
        });
        ui.separator();

        let available = ui.available_rect_before_wrap();
        let list_width = (available.width() * 0.31).clamp(150.0, 300.0);
        let list_rect = Rect::from_min_max(
            available.min,
            egui::pos2(available.min.x + list_width, available.max.y),
        );
        let detail_rect = Rect::from_min_max(
            egui::pos2(list_rect.max.x + 15.0, available.min.y),
            available.max,
        );
        let mut preview = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("discovered-cameras")
                .show(ui, |ui| {
                    for (index, draft) in self.drafts.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut draft.checked, "");
                            if ui
                                .selectable_label(self.selected == Some(index), &draft.camera.name)
                                .clicked()
                            {
                                preview = Some(index);
                            }
                        });
                        ui.small(&draft.note);
                        ui.separator();
                    }
                    if self.drafts.is_empty() {
                        ui.label("Cameras will appear here as they respond.");
                    }
                });
        });

        let mut edited = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(detail_rect), |ui| {
            let Some(index) = self.selected else { return };
            let draft = &mut self.drafts[index];
            egui::Grid::new("discovered-camera-details")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut draft.camera.name);
                    ui.end_row();
                    ui.label("Address");
                    edited |= ui.text_edit_singleline(&mut draft.camera.url).changed();
                    ui.end_row();
                    ui.label("Username");
                    edited |= ui.text_edit_singleline(&mut draft.username).changed();
                    ui.end_row();
                    ui.label("Password variable");
                    edited |= ui.text_edit_singleline(&mut draft.password_env).changed();
                    ui.end_row();
                });
            ui.small("Use a password environment variable, as in config.toml. Leave credentials blank for open feeds.");
            if ui.button("Preview").clicked() {
                preview = Some(index);
            }
            if let Some(player) = &self.preview {
                let (frame, status, detail) =
                    player.read(|s| (s.frame.clone(), s.status, s.detail.clone()));
                ui.label(format!("{status:?}"));
                if let Some(detail) = detail {
                    ui.colored_label(Color32::YELLOW, detail);
                }
                if let Some(error) = state
                    .renderer
                    .read()
                    .callback_resources
                    .get::<VideoRenderer>()
                    .and_then(|r| r.errors.get(&0))
                    .cloned()
                {
                    ui.colored_label(Color32::YELLOW, error);
                }
                let (rect, _) = ui.allocate_exact_size(
                    ui.available_size().max(Vec2::splat(1.0)), Sense::hover(),
                );
                ui.painter().rect_filled(rect, 4, Color32::BLACK);
                if let Some(frame) = frame {
                    let aspect = frame.width as f32 / frame.height as f32;
                    let size = Vec2::new(
                        rect.width().min(rect.height() * aspect),
                        rect.height().min(rect.width() / aspect),
                    );
                    let rect = Rect::from_center_size(rect.center(), size);
                    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                        rect, VideoCallback { id: 0, frame },
                    ));
                }
            } else {
                ui.label("Choose Preview to connect using these settings.");
            }
        });
        if edited {
            self.preview = None;
        }
        if let Some(index) = preview {
            self.start_preview(index, &context);
        }
        state
            .renderer
            .write()
            .callback_resources
            .get_mut::<VideoRenderer>()
            .expect("video renderer")
            .retain(if self.preview.is_some() { &[0] } else { &[] });
        saved
    }

    pub fn shutdown(self) {
        if let Some(scan) = &self.scan {
            scan.stop();
        }
        if let Some(preview) = &self.preview {
            preview.stop();
        }
        if let Some(scan) = self.scan {
            scan.join();
        }
        if let Some(preview) = self.preview {
            preview.join();
        }
    }
}

fn default_range(interface: &Interface) -> String {
    Subnet {
        address: interface.address,
        prefix: interface.subnet.prefix.max(24),
    }
    .cidr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, tests::paint},
        render::tests::gpu,
        stream::Status,
    };
    use std::time::{Duration, Instant};

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(12);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "Discovery preview did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh discovery, camera and GPU fixtures"]
    fn discovers_previews_and_saves_only_selected_cameras() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = "# Existing camera must survive\n[[cameras]]\nname='Existing'\ntype='rtsp'\nurl='rtsp://127.0.0.1:18554/sub'\n";
        std::fs::write(&path, original).unwrap();
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::dark());
        let state = gpu();
        state
            .renderer
            .write()
            .callback_resources
            .insert(VideoRenderer::new(&state.device, state.target_format).unwrap());
        let mut discovery = DiscoveryUi {
            path: path.clone(),
            interfaces: vec![],
            interface: 0,
            range: "127.0.0.1/32".into(),
            scan: None,
            drafts: vec![],
            selected: None,
            preview: None,
            error: None,
            existing: Config::load(&path).unwrap().cameras,
        };
        discovery.start_plan(Plan::fixture(), &context).unwrap();
        wait_until(|| !discovery.scan.as_ref().unwrap().snapshot().active);
        discovery.poll();
        assert_eq!(discovery.drafts.len(), 2);
        for kind in [crate::config::Kind::Rtsp, crate::config::Kind::Onvif] {
            let index = discovery
                .drafts
                .iter()
                .position(|d| d.camera.kind == kind)
                .unwrap();
            if kind == crate::config::Kind::Onvif {
                discovery.drafts[index].username = "viewer".into();
                discovery.drafts[index].password_env = "CAMCAM_FIXTURE_PASSWORD".into();
            }
            discovery.start_preview(index, &context);
            wait_until(|| {
                discovery
                    .preview
                    .as_ref()
                    .unwrap()
                    .read(|s| s.status == Status::Live)
            });
            paint(
                &context,
                &state,
                Vec::new(),
                Some(if kind == crate::config::Kind::Rtsp {
                    "discover-rtsp"
                } else {
                    "discover-onvif"
                }),
                |ui| {
                    discovery.show(ui, &state);
                },
            );
        }
        let index = discovery
            .drafts
            .iter()
            .position(|d| d.camera.kind == crate::config::Kind::Onvif)
            .unwrap();
        // Drive the checkbox and save button through the same pointer events as the app.
        let checkbox = egui::pos2(7.0, 111.0 + index as f32 * 44.0);
        for pressed in [true, false] {
            paint(
                &context,
                &state,
                vec![
                    egui::Event::PointerMoved(checkbox),
                    egui::Event::PointerButton {
                        pos: checkbox,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
                None,
                |ui| {
                    discovery.show(ui, &state);
                },
            );
        }
        assert!(discovery.drafts[index].checked);
        // Let egui register the newly enabled save button before pressing it.
        paint(&context, &state, Vec::new(), None, |ui| {
            discovery.show(ui, &state);
        });
        let save = egui::pos2(90.0, 82.0);
        let mut saved = false;
        for pressed in [true, false] {
            paint(
                &context,
                &state,
                vec![
                    egui::Event::PointerMoved(save),
                    egui::Event::PointerButton {
                        pos: save,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
                None,
                |ui| {
                    saved |= discovery.show(ui, &state);
                },
            );
        }
        assert!(saved, "{:?}", discovery.error);
        assert_eq!(discovery.save().unwrap(), 0);
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.cameras.len(), 2);
        assert_eq!(saved.cameras[0].name, "Existing");
        assert_eq!(
            saved.cameras[1].password_env.as_deref(),
            Some("CAMCAM_FIXTURE_PASSWORD")
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# Existing camera must survive")
        );
        // Editing a discovered URL must not cause its original scan result to reappear.
        discovery.drafts[index].camera.url.push_str("?edited");
        discovery.poll();
        assert_eq!(discovery.drafts.len(), 2);
        discovery.shutdown();
        let mut creation = eframe::CreationContext::_new_kittest(context.clone());
        creation.wgpu_render_state = Some(state.clone());
        let mut app = App::new(&creation, &path).unwrap();
        paint(&context, &state, Vec::new(), None, |ui| app.show(ui));
        eframe::App::on_exit(&mut app);
    }
}
