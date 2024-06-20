//! This is based on the `boids` example from `wgpu`. Credit to https://github.com/gfx-rs/wgpu authors.

// Flocking boids example with gpu compute update pass
// adapted from https://github.com/austinEng/webgpu-samples/blob/master/src/examples/computeBoids.ts

use std::sync::Arc;

use futures::{pin_mut, FutureExt};
use tww::Result;
use winit::keyboard::{Key, NamedKey};

pub fn main() -> tww::FinishResult<(), tww::TwwError> {
    pretty_env_logger::init();

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::util::backend_bits_from_env().unwrap_or_default(),
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        dx12_shader_compiler: wgpu::util::dx12_shader_compiler_from_env().unwrap_or_default(),
        gles_minor_version: wgpu::util::gles_minor_version_from_env().unwrap_or_default(),
    });

    tww::start(instance, run)?;
    Ok(())
}

pub async fn run(instance: Arc<wgpu::Instance>) -> Result<()> {
    // Create the window and retrieve its surface.
    let window = tww::Window::new().await?;
    let mut dimensions_watcher = window.dimensions_watcher();
    let mut keyboard_listener = window.keyboard_listener();
    let surface = window.create_surface().await?;

    // Acquire the adapter.
    log::trace!("acquiring adapter");
    let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, Some(&surface))
        .await
        .expect("could not select an adapter");
    let adapter_info = adapter.get_info();
    log::info!(
        "adapter {} ({:?}) acquired",
        adapter_info.name,
        adapter_info.backend
    );

    log::trace!("checking adapter features for compatibility");
    // TODO: What features are required?
    let minimum_features = wgpu::Features::empty();
    let adapter_features = adapter.features();
    assert!(
        adapter_features.contains(minimum_features),
        "adapter does not support minimum features: {:?}",
        minimum_features - adapter_features
    );

    log::trace!("checking adapter capabilities for compatibility");
    let required_downlevel_capabilities = wgpu::DownlevelCapabilities {
        flags: wgpu::DownlevelFlags::COMPUTE_SHADERS,
        ..Default::default()
    };
    let downlevel_capabilities = adapter.get_downlevel_capabilities();
    assert!(
        downlevel_capabilities.shader_model >= required_downlevel_capabilities.shader_model,
        "adapter does not support the minimum shader model required to run this example: {:?}",
        required_downlevel_capabilities.shader_model
    );
    assert!(
        downlevel_capabilities
            .flags
            .contains(required_downlevel_capabilities.flags),
        "adapter does not support the downlevel capabilities required to run this example: {:?}",
        required_downlevel_capabilities.flags - downlevel_capabilities.flags
    );

    log::trace!("acquiring device");
    // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the surface.
    let required_limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
    // TODO: Which features are optional?
    let optional_features = wgpu::Features::empty();
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: (optional_features & adapter_features) | minimum_features,
                required_limits,
            },
            None,
        )
        .await
        .expect("could not find a suitable GPU device");

    let device = Arc::new(device);
    let queue = Arc::new(queue);

    log::trace!("configure window surface");
    let window_size: winit::dpi::PhysicalSize<u32> = dimensions_watcher.dimensions();
    log::info!("window size in pixels: {window_size:?}");
    let width = window_size.width.max(1);
    let height = window_size.height.max(1);
    // Get the default configuration.
    let mut surface_config = surface
        .get_default_config(&adapter, width, height)
        .expect("surface isn't supported by the adapter");
    // All platforms support non-sRGB swapchains, so we can just use the format directly.
    let target_format = surface_config.format.remove_srgb_suffix();
    surface_config.format = target_format;
    surface_config.view_formats.push(target_format);
    surface.configure(&device, &surface_config);

    let mut egui_renderer = window.create_egui_renderer(
        device.clone(),
        queue.clone(),
        target_format,
        wgpu::Color::GREEN,
        None,
    );

    let redraw = window.redraw().fuse();
    pin_mut!(redraw);

    loop {
        futures::select_biased!(
            dimensions = dimensions_watcher.wait_dimensions().fuse() => {
                let dimensions = dimensions?;
                surface_config.width = dimensions.width.max(1);
                surface_config.height = dimensions.height.max(1);
                surface.configure(&device, &surface_config);
                drop(egui_renderer);
                egui_renderer =
                    window.create_egui_renderer(device.clone(), queue.clone(), target_format, wgpu::Color::GREEN, None);
                log::info!("resized");
            }
            event = keyboard_listener.wait_event().fuse() => {
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    return Ok(());
                }
            }
            _ = redraw => {
                egui_renderer.ui(|ctx| {
                    egui::CentralPanel::default().show(&ctx, ui);
                });
                if let Err(e) = egui_renderer.render(&surface, None) {
                    log::error!("error: {e}");
                }
                // Attempt another redraw immediately.
                redraw.set(window.redraw().fuse());
            }
        );
    }
}

fn ui(ui: &mut egui::Ui) {
    log::info!("size: {}", ui.available_size());
    ui.horizontal(|ui| {
        ui.label("Hello world!");
        if ui.button("Click me").clicked() {
            log::info!("clicked");
        }
    });
}
