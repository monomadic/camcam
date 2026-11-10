use camcam::{app::App, config::Config};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut path = None;
    let mut discover = false;
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--discover" => discover = true,
            "--config" => {
                let Some(value) = arguments.next().filter(|value| !value.starts_with("--")) else {
                    usage_error()
                };
                path = Some(if let Some(relative) = value.strip_prefix("~/") {
                    PathBuf::from(
                        std::env::var_os("HOME")
                            .ok_or_else(|| anyhow::anyhow!("HOME is unavailable"))?,
                    )
                    .join(relative)
                } else {
                    PathBuf::from(value)
                });
            }
            "--help" | "-h" => {
                println!("Usage: camcam [--discover] [--config PATH]");
                println!(
                    "  --discover  Find local ONVIF/RTSP cameras, preview, and save selected cameras."
                );
                return Ok(());
            }
            _ => usage_error(),
        }
    }
    let path = match path {
        Some(path) => path,
        None => Config::default_path()?,
    };
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = wgpu::Backends::METAL;
    setup.device_descriptor = std::sync::Arc::new(|_| wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
        ..Default::default()
    });
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("CamCam")
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([460.0, 320.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
            surface: egui_wgpu::SurfaceConfig::LOW_LATENCY,
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "CamCam",
        options,
        Box::new(move |context| {
            let mut app = if discover {
                App::discover(context, &path)?
            } else {
                App::new(context, &path)?
            };
            app.install_menu(&context.egui_ctx)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn usage_error() -> ! {
    eprintln!("Usage: camcam [--discover] [--config PATH]");
    std::process::exit(2);
}
