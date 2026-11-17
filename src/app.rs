use crate::{
    config::{Camera, Config},
    discover_ui::DiscoveryUi,
    render::{VideoCallback, VideoRenderer},
    stream::{Player, Source, Status},
};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Vec2};
use std::path::Path;

struct CameraFeed {
    name: String,
    wall: Player,
    focus: Option<Player>,
}

pub struct App {
    feeds: Vec<CameraFeed>,
    selected: Option<usize>,
    empty_message: Option<String>,
    render_state: egui_wgpu::RenderState,
    discovery: Option<DiscoveryUi>,
    config_path: std::path::PathBuf,
    menu: Option<crate::menu::NativeMenu>,
}

impl App {
    pub fn new(context: &eframe::CreationContext<'_>, path: &Path) -> anyhow::Result<Self> {
        Self::create(context, path, false)
    }

    pub fn discover(context: &eframe::CreationContext<'_>, path: &Path) -> anyhow::Result<Self> {
        Self::create(context, path, true)
    }

    fn create(
        context: &eframe::CreationContext<'_>,
        path: &Path,
        discover: bool,
    ) -> anyhow::Result<Self> {
        let state = context
            .wgpu_render_state
            .as_ref()
            .expect("wgpu backend")
            .clone();
        let renderer = VideoRenderer::new(&state.device, state.target_format)?;
        state.renderer.write().callback_resources.insert(renderer);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::TRANSPARENT;
        visuals.window_fill = Color32::TRANSPARENT;
        context.egui_ctx.set_visuals(visuals);
        let (cameras, empty_message) = match Config::load(path) {
            Ok(config) => (config.cameras, None),
            Err(_) => {
                eprintln!("CamCam: could not load camera configuration");
                (
                    Vec::new(),
                    Some(format!(
                        "Could not read camera configuration.\n{}",
                        path.display()
                    )),
                )
            }
        };
        let feeds = cameras
            .into_iter()
            .map(|camera| Self::feed(camera, &context.egui_ctx, path))
            .collect();
        Ok(Self {
            feeds,
            selected: None,
            empty_message,
            render_state: state,
            discovery: discover.then(|| DiscoveryUi::new(path, &context.egui_ctx)),
            config_path: path.to_owned(),
            menu: None,
        })
    }

    pub fn install_menu(&mut self, context: &egui::Context) -> anyhow::Result<()> {
        self.menu = Some(crate::menu::NativeMenu::new(context)?);
        Ok(())
    }

    fn open_scanner(&mut self, context: &egui::Context) {
        if self.discovery.is_none() {
            self.discovery = Some(DiscoveryUi::new(&self.config_path, context));
        }
    }

    fn feed(camera: Camera, context: &egui::Context, path: &Path) -> CameraFeed {
        let name = camera.name.clone();
        let context = context.clone();
        CameraFeed {
            name,
            wall: Player::saved(camera, path, move || context.request_repaint()),
            focus: None,
        }
    }

    fn select(&mut self, selection: Option<usize>) {
        if self.selected == selection {
            return;
        }
        if let Some(previous) = self.selected {
            self.feeds[previous].focus = None;
        }
        self.selected = selection;
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        if self.menu.as_ref().is_some_and(|menu| menu.take_scan()) {
            self.open_scanner(&context);
        }
        if context.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Q)) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.discovery.is_some()
            && (ui.button("← Back to wall").clicked()
                || context.input(|i| i.key_pressed(egui::Key::Escape)))
        {
            if let Some(discovery) = self.discovery.take() {
                discovery.shutdown();
            }
            context.request_repaint();
        }
        if let Some(discovery) = &mut self.discovery {
            let saved = egui::Frame::new()
                .inner_margin(18.0)
                .show(ui, |ui| discovery.show(ui, &self.render_state))
                .inner;
            if saved {
                self.select(None);
                self.discovery = None;
                match Config::load(&self.config_path) {
                    Ok(config) => {
                        self.feeds = config
                            .cameras
                            .into_iter()
                            .map(|camera| Self::feed(camera, &context, &self.config_path))
                            .collect();
                        self.empty_message = None;
                    }
                    Err(_) => {
                        self.empty_message = Some("Could not reload camera configuration.".into())
                    }
                }
                context.request_repaint();
            }
            return;
        }
        if context.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.select(None);
        }
        if let Some(index) = self.selected {
            let feed = &mut self.feeds[index];
            if feed.wall.read(|s| s.streams.is_none()) {
                feed.focus = None;
            }
            if feed.focus.is_none()
                && let Some(url) = feed
                    .wall
                    .read(|s| s.streams.as_ref().and_then(|s| s.focus.clone()))
            {
                let context = context.clone();
                feed.focus = Some(Player::new(Source::Direct(url), move || {
                    context.request_repaint()
                }));
            }
        }

        let full = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(full, 0, Color32::from_rgba_unmultiplied(0, 0, 0, 190));
        let content = full.shrink(18.0);
        let heading = if self.selected.is_some() {
            "CAMERA FOCUS"
        } else {
            "CAMERA WALL"
        };
        painter.text(
            content.left_top(),
            Align2::LEFT_TOP,
            heading,
            FontId::monospace(14.0),
            Color32::from_gray(225),
        );
        let hint = if self.selected.is_some() {
            "ESC  /  WALL".to_owned()
        } else {
            format!("{} CAMERAS", self.feeds.len())
        };
        painter.text(
            content.right_top(),
            Align2::RIGHT_TOP,
            hint,
            FontId::monospace(11.0),
            Color32::GRAY,
        );
        let content = Rect::from_min_max(content.min + Vec2::new(0.0, 30.0), content.max);
        if self.feeds.is_empty() {
            let message = self
                .empty_message
                .as_deref()
                .unwrap_or("Choose Tools → Scan… to find cameras, or add them to ~/.config/camcam/config.toml.");
            painter.text(
                content.center(),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(15.0),
                Color32::GRAY,
            );
            return;
        }

        let aspects: Vec<_> = self
            .feeds
            .iter()
            .map(|feed| {
                feed.wall.read(|state| {
                    state
                        .frame
                        .as_ref()
                        .map(|frame| frame.width as f32 / frame.height as f32)
                        .unwrap_or(16.0 / 9.0)
                })
            })
            .collect();
        let tiles = layout(content, &aspects, self.selected);
        let mut active = Vec::new();
        let mut clicked = None;
        for (index, rect) in tiles {
            let feed = &self.feeds[index];
            let (mut frame, mut status, mut detail) = feed
                .wall
                .read(|s| (s.frame.clone(), s.status, s.detail.clone()));
            let mut id = index * 2;
            if let Some(focus) = &feed.focus {
                let (main, main_detail) = focus.read(|s| (s.frame.clone(), s.detail.clone()));
                if let Some(main) = main {
                    frame = Some(main);
                    status = Status::Live;
                    id += 1;
                } else if let Some(error) = main_detail {
                    detail = Some(format!("Main stream: {error}"));
                }
            }
            active.push(id);
            let render_error = self
                .render_state
                .renderer
                .read()
                .callback_resources
                .get::<VideoRenderer>()
                .and_then(|r| r.errors.get(&id))
                .cloned();
            if let Some(error) = render_error {
                detail = Some(error);
            }
            let (label, color) = match status {
                Status::Connecting => ("CONNECTING", Color32::YELLOW),
                Status::Live => ("LIVE · VT", Color32::from_rgb(75, 205, 110)),
                Status::Retrying | Status::Ended => ("RETRYING", Color32::from_rgb(245, 160, 70)),
            };
            let painter = ui.painter();
            painter.rect_filled(rect, 6, Color32::from_gray(22));
            let video = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.max.y - 35.0));
            if let Some(frame) = frame {
                let video = fit(
                    video,
                    frame.width as f32 / frame.height as f32,
                    self.selected.is_none(),
                );
                painter.add(egui_wgpu::Callback::new_paint_callback(
                    video,
                    VideoCallback {
                        id,
                        frame,
                        size: video.size(),
                        corner_radius: 6.0,
                    },
                ));
            }
            let footer = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - 35.0), rect.max);
            painter.circle_filled(footer.left_center() + Vec2::new(15.0, 0.0), 3.0, color);
            let status_galley =
                painter.layout_no_wrap(label.into(), FontId::monospace(10.0), color);
            let status_width = status_galley.size().x.min(rect.width() * 0.46);
            let status_rect = Rect::from_min_size(
                Pos2::new(footer.max.x - status_width - 12.0, footer.center().y - 7.0),
                Vec2::new(status_width, 16.0),
            );
            painter
                .with_clip_rect(status_rect)
                .galley(status_rect.min, status_galley, color);
            let name_rect = Rect::from_min_max(
                footer.min + Vec2::new(28.0, 8.0),
                Pos2::new(status_rect.min.x - 8.0, footer.max.y),
            );
            painter.with_clip_rect(name_rect).text(
                name_rect.min,
                Align2::LEFT_TOP,
                &feed.name,
                FontId::proportional(13.0),
                Color32::from_gray(230),
            );
            let response = ui.interact(rect, egui::Id::new(("camera", index)), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("{} — {label}", feed.name),
                )
            });
            if response.clicked() {
                clicked = Some(index);
            }
            if let Some(detail) = detail {
                response.on_hover_text(detail);
            }
        }
        self.render_state
            .renderer
            .write()
            .callback_resources
            .get_mut::<VideoRenderer>()
            .expect("video renderer")
            .retain(&active);
        if let Some(index) = clicked {
            self.select(if self.selected == Some(index) {
                None
            } else {
                Some(index)
            });
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.show(ui);
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self) {
        if let Some(discovery) = self.discovery.take() {
            discovery.shutdown();
        }
        // Signal all workers first, then join. No UI-thread join during focus switching.
        for feed in &self.feeds {
            feed.wall.stop();
            if let Some(focus) = &feed.focus {
                focus.stop();
            }
        }
        for feed in self.feeds.drain(..) {
            feed.wall.join();
            if let Some(focus) = feed.focus {
                focus.join();
            }
        }
    }
}

pub fn layout(content: Rect, aspects: &[f32], selected: Option<usize>) -> Vec<(usize, Rect)> {
    if let Some(index) = selected {
        return vec![(index, content)];
    }
    let count = aspects.len();
    if count == 0 {
        return Vec::new();
    }
    let columns = (1..=count)
        .max_by(|a, b| {
            tile_width(content, aspects, *a).total_cmp(&tile_width(content, aspects, *b))
        })
        .unwrap_or(1);
    let width = tile_width(content, aspects, columns);
    let rows = count.div_ceil(columns);
    let mut tiles = Vec::with_capacity(count);
    let mut top = content.top();
    for row in 0..rows {
        let first = row * columns;
        let last = (first + columns).min(count);
        for index in first..last {
            let height = width / aspects[index].max(0.01) + 35.0;
            let min = Pos2::new(
                content.left() + (index - first) as f32 * (width + 10.0),
                top,
            );
            tiles.push((index, Rect::from_min_size(min, Vec2::new(width, height))));
        }
        top += tiles[first..last]
            .iter()
            .map(|(_, rect)| rect.height())
            .fold(0.0, f32::max)
            + 10.0;
    }
    tiles
}

fn tile_width(content: Rect, aspects: &[f32], columns: usize) -> f32 {
    let rows = aspects.len().div_ceil(columns);
    let by_width = (content.width() - (columns - 1) as f32 * 10.0) / columns as f32;
    let inverse_aspects: f32 = aspects
        .chunks(columns)
        .map(|row| {
            row.iter()
                .map(|aspect| 1.0 / aspect.max(0.01))
                .fold(0.0, f32::max)
        })
        .sum();
    let by_height =
        (content.height() - (rows - 1) as f32 * 10.0 - rows as f32 * 35.0) / inverse_aspects;
    by_width.min(by_height).max(0.0)
}

fn fit(rect: Rect, aspect: f32, align_top: bool) -> Rect {
    let size = Vec2::new(
        rect.width().min(rect.height() * aspect),
        rect.height().min(rect.width() / aspect),
    )
    .max(Vec2::ZERO);
    let top = if align_top {
        rect.top()
    } else {
        rect.center().y - size.y / 2.0
    };
    Rect::from_min_size(Pos2::new(rect.center().x - size.x / 2.0, top), size)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::render::tests::{gpu, readback};
    use std::time::{Duration, Instant};

    fn pass(app: &mut App, context: &egui::Context, events: Vec<egui::Event>, image: Option<&str>) {
        let state = app.render_state.clone();
        paint(context, &state, events, image, |ui| app.show(ui));
    }

    pub(crate) fn paint(
        context: &egui::Context,
        state: &egui_wgpu::RenderState,
        events: Vec<egui::Event>,
        image: Option<&str>,
        mut show: impl FnMut(&mut egui::Ui),
    ) {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                events,
                ..Default::default()
            },
            |ui| show(ui),
        );
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let mut renderer = state.renderer.write();
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(&state.device, &state.queue, *id, delta);
            }
        }
        let pixels = readback(state, 1000, 640, |view, encoder| {
            let buffers = renderer.update_buffers(
                &state.device,
                &state.queue,
                encoder,
                &jobs,
                &egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [1000, 640],
                    pixels_per_point: output.pixels_per_point,
                },
            );
            state.queue.submit(buffers);
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render(
                &mut pass,
                &jobs,
                &egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [1000, 640],
                    pixels_per_point: output.pixels_per_point,
                },
            );
        });
        for id in &output.textures_delta.free {
            renderer.free_texture(id);
        }
        output.textures_delta.clear();
        assert!(
            renderer
                .callback_resources
                .get::<VideoRenderer>()
                .unwrap()
                .errors
                .is_empty()
        );
        if let Some(name) = image {
            std::fs::create_dir_all("target/verification").unwrap();
            image::save_buffer(
                format!("target/verification/{name}.png"),
                &pixels,
                1000,
                640,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "Camera state did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn grid_and_focus_keep_camera_order() {
        let content = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        let aspects = [16.0 / 9.0; 4];
        let wall = layout(content, &aspects, None);
        assert_eq!(
            wall.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert!(wall[0].1.right() < wall[1].1.left());
        assert!(wall[0].1.bottom() < wall[2].1.top());
        for (index, rect) in &wall {
            assert!((rect.height() - 35.0 - rect.width() / aspects[*index]).abs() < 0.001);
        }
        assert_eq!(layout(content, &aspects, Some(2)), [(2, content)]);

        let mixed = layout(content, &[16.0 / 9.0, 4.0 / 3.0, 9.0 / 16.0], None);
        for (index, rect) in mixed {
            let aspect = [16.0 / 9.0, 4.0 / 3.0, 9.0 / 16.0][index];
            assert!((rect.height() - 35.0 - rect.width() / aspect).abs() < 0.001);
        }
    }

    #[test]
    fn thumbnails_keep_aspect_ratio_and_align_to_top() {
        let tile = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(300.0, 300.0));
        let wide = fit(tile, 16.0 / 9.0, true);
        assert_eq!(wide.top(), tile.top());
        assert_eq!(wide.center().x, tile.center().x);
        assert!((wide.width() / wide.height() - 16.0 / 9.0).abs() < 0.001);

        let portrait = fit(tile, 9.0 / 16.0, true);
        assert_eq!(portrait.top(), tile.top());
        assert_eq!(portrait.center().x, tile.center().x);
        assert!((portrait.width() / portrait.height() - 9.0 / 16.0).abs() < 0.001);

        let focused = fit(tile, 16.0 / 9.0, false);
        assert_eq!(focused.center(), tile.center());
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures and macOS GPU access"]
    fn four_feeds_focus_main_stream_and_escape_through_normal_ui() {
        let context = egui::Context::default();
        let mut creation = eframe::CreationContext::_new_kittest(context.clone());
        creation.wgpu_render_state = Some(gpu());
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::copy("tests/fixtures/wall.toml", &config_path).unwrap();
        let mut app = App::new(&creation, &config_path).unwrap();
        assert_eq!(app.feeds.len(), 4);
        wait_until(|| {
            app.feeds
                .iter()
                .all(|f| f.wall.read(|s| s.status == Status::Live))
        });
        pass(&mut app, &context, Vec::new(), Some("wall"));
        let content = Rect::from_min_max(Pos2::new(18.0, 48.0), Pos2::new(982.0, 622.0));
        let point = layout(content, &[16.0 / 9.0; 4], None)[1].1.center();
        for pressed in [true, false] {
            pass(
                &mut app,
                &context,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
                None,
            );
        }
        assert_eq!(app.selected, Some(1));
        pass(&mut app, &context, Vec::new(), None);
        wait_until(|| {
            app.feeds[1]
                .focus
                .as_ref()
                .unwrap()
                .read(|s| s.frame.as_ref().is_some_and(|f| f.width == 1280))
        });
        pass(&mut app, &context, Vec::new(), Some("focus"));
        // Returning to the wall releases the additional main-stream worker.
        pass(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            None,
        );
        assert!(app.selected.is_none());
        assert!(app.feeds[1].focus.is_none());
        assert!(
            app.feeds
                .iter()
                .all(|f| f.wall.read(|s| s.status == Status::Live))
        );
        eframe::App::on_exit(&mut app);
    }
}
