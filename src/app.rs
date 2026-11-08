use crate::{
    config::{Camera, Config},
    render::{VideoCallback, VideoRenderer},
    stream::{Player, Source, Status},
};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
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
}

impl App {
    pub fn new(context: &eframe::CreationContext<'_>, path: &Path) -> anyhow::Result<Self> {
        let state = context
            .wgpu_render_state
            .as_ref()
            .expect("wgpu backend")
            .clone();
        let renderer = VideoRenderer::new(&state.device, state.target_format)?;
        state.renderer.write().callback_resources.insert(renderer);
        context.egui_ctx.set_visuals(egui::Visuals::dark());
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
            .map(|camera| Self::feed(camera, &context.egui_ctx))
            .collect();
        Ok(Self {
            feeds,
            selected: None,
            empty_message,
            render_state: state,
        })
    }

    fn feed(camera: Camera, context: &egui::Context) -> CameraFeed {
        let name = camera.name.clone();
        let context = context.clone();
        CameraFeed {
            name,
            wall: Player::new(Source::Camera(camera), move || context.request_repaint()),
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
        if context.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.select(None);
        }
        if context.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Q)) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if let Some(index) = self.selected {
            let feed = &mut self.feeds[index];
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
        painter.rect_filled(full, 0, Color32::from_gray(11));
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
                .unwrap_or("Add cameras to ~/.config/camcam/config.toml and relaunch.");
            painter.text(
                content.center(),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(15.0),
                Color32::GRAY,
            );
            return;
        }

        let tiles = layout(content, self.feeds.len(), self.selected);
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
            painter.rect_filled(video, 0, Color32::BLACK);
            if let Some(frame) = frame {
                let video = fit(video, frame.width as f32 / frame.height as f32);
                painter.add(egui_wgpu::Callback::new_paint_callback(
                    video,
                    VideoCallback { id, frame },
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
            let focused = self.selected == Some(index);
            painter.rect_stroke(
                rect,
                6,
                Stroke::new(
                    if focused { 2.0 } else { 1.0 },
                    if focused {
                        Color32::from_rgb(60, 145, 255)
                    } else {
                        Color32::from_gray(55)
                    },
                ),
                egui::StrokeKind::Inside,
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

    fn on_exit(&mut self) {
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

pub fn layout(content: Rect, count: usize, selected: Option<usize>) -> Vec<(usize, Rect)> {
    if let Some(index) = selected {
        return vec![(index, content)];
    }
    if count == 0 {
        return Vec::new();
    }
    let columns = (1..=count)
        .max_by(|a, b| {
            let video_width = |columns: usize| {
                let rows = count.div_ceil(columns);
                let width = (content.width() - (columns - 1) as f32 * 10.0) / columns as f32;
                let height = (content.height() - (rows - 1) as f32 * 10.0) / rows as f32;
                width.min((height - 35.0) * 16.0 / 9.0)
            };
            video_width(*a).total_cmp(&video_width(*b))
        })
        .unwrap_or(1);
    let rows = count.div_ceil(columns);
    let width = ((content.width() - (columns - 1) as f32 * 10.0) / columns as f32).max(0.0);
    let height = ((content.height() - (rows - 1) as f32 * 10.0) / rows as f32).max(0.0);
    (0..count)
        .map(|index| {
            let min = content.min
                + Vec2::new(
                    (index % columns) as f32 * (width + 10.0),
                    (index / columns) as f32 * (height + 10.0),
                );
            (index, Rect::from_min_size(min, Vec2::new(width, height)))
        })
        .collect()
}

fn fit(rect: Rect, aspect: f32) -> Rect {
    let size = Vec2::new(
        rect.width().min(rect.height() * aspect),
        rect.height().min(rect.width() / aspect),
    )
    .max(Vec2::ZERO);
    Rect::from_center_size(rect.center(), size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::{gpu, readback};
    use std::time::{Duration, Instant};

    fn pass(app: &mut App, context: &egui::Context, events: Vec<egui::Event>, image: Option<&str>) {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))),
                events,
                ..Default::default()
            },
            |ui| app.show(ui),
        );
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let state = &app.render_state;
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
        let wall = layout(content, 4, None);
        assert_eq!(
            wall.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert!(wall[0].1.right() < wall[1].1.left());
        assert!(wall[0].1.bottom() < wall[2].1.top());
        assert_eq!(layout(content, 4, Some(2)), [(2, content)]);
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures and macOS GPU access"]
    fn four_feeds_focus_main_stream_and_escape_through_normal_ui() {
        let context = egui::Context::default();
        let mut creation = eframe::CreationContext::_new_kittest(context.clone());
        creation.wgpu_render_state = Some(gpu());
        let mut app = App::new(&creation, Path::new("tests/fixtures/wall.toml")).unwrap();
        assert_eq!(app.feeds.len(), 4);
        wait_until(|| {
            app.feeds
                .iter()
                .all(|f| f.wall.read(|s| s.status == Status::Live))
        });
        pass(&mut app, &context, Vec::new(), Some("wall"));
        let content = Rect::from_min_max(Pos2::new(18.0, 48.0), Pos2::new(982.0, 622.0));
        let point = layout(content, 4, None)[1].1.center();
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
