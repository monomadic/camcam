use camcam::{app::App, config::Config};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = match args.as_slice() {
        [] => Config::default_path()?,
        [help] if help == "--help" || help == "-h" => {
            println!("Usage: camcam [--config PATH]");
            return Ok(());
        }
        [option, path] if option == "--config" => {
            if let Some(path) = path.strip_prefix("~/") {
                PathBuf::from(
                    std::env::var_os("HOME")
                        .ok_or_else(|| anyhow::anyhow!("HOME is unavailable"))?,
                )
                .join(path)
            } else {
                PathBuf::from(path)
            }
        }
        _ => {
            eprintln!("Usage: camcam [--config PATH]");
            std::process::exit(2);
        }
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
        Box::new(move |context| Ok(Box::new(App::new(context, &path)?))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}
