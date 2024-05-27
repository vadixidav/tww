//! This is based on the `boids` example from `wgpu`. Credit to https://github.com/gfx-rs/wgpu authors.

// Flocking boids example with gpu compute update pass
// adapted from https://github.com/austinEng/webgpu-samples/blob/master/src/examples/computeBoids.ts

use std::{borrow::Cow, mem, sync::Arc};

use nanorand::{Rng, WyRand};
use tww::Result;
use wgpu::util::DeviceExt;

const NUM_PARTICLES: u32 = 1500;
const PARTICLES_PER_GROUP: u32 = 64;

pub fn main() -> tww::FinishResult<(), tww::TwwError> {
    pretty_env_logger::init();

    let instance = Arc::new(wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::util::backend_bits_from_env().unwrap_or_default(),
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        dx12_shader_compiler: wgpu::util::dx12_shader_compiler_from_env().unwrap_or_default(),
        gles_minor_version: wgpu::util::gles_minor_version_from_env().unwrap_or_default(),
    }));

    tww::start(instance.clone(), run(instance))?;
    Ok(())
}

pub async fn run(instance: Arc<wgpu::Instance>) -> Result<()> {
    let mut lifecycle = tww::LifecycleWatcher::new();

    // Wait for the rendering stage, at which point we can create windows.
    lifecycle.wait_rendering().await;

    // Create the window and retrieve its surface.
    let window = tww::Window::new().await?;
    let mut dimensions_watcher = window.dimensions_watcher();
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

    log::trace!("configure window surface");
    let window_size = dimensions_watcher.dimensions();
    log::info!("window size in pixels: {window_size:?}");
    let width = window_size.width.max(1);
    let height = window_size.height.max(1);
    // Get the default configuration.
    let mut surface_config = surface
        .get_default_config(&adapter, width, height)
        .expect("surface isn't supported by the adapter");
    // All platforms support non-sRGB swapchains, so we can just use the format directly.
    let format = surface_config.format.remove_srgb_suffix();
    surface_config.format = format;
    surface_config.view_formats.push(format);
    surface.configure(&device, &surface_config);

    // TODO: maybe window.request_redraw();

    let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("compute.wgsl"))),
    });
    let draw_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("draw.wgsl"))),
    });

    // buffer for simulation parameters uniform

    let sim_param_data = [
        0.04f32, // deltaT
        0.1,     // rule1Distance
        0.025,   // rule2Distance
        0.025,   // rule3Distance
        0.02,    // rule1Scale
        0.05,    // rule2Scale
        0.005,   // rule3Scale
    ]
    .to_vec();
    let sim_param_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Simulation Parameter Buffer"),
        contents: bytemuck::cast_slice(&sim_param_data),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    // create compute bind layout group and compute pipeline layout

    let compute_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            (sim_param_data.len() * mem::size_of::<f32>()) as _,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new((NUM_PARTICLES * 16) as _),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new((NUM_PARTICLES * 16) as _),
                    },
                    count: None,
                },
            ],
            label: None,
        });
    let compute_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("compute"),
        bind_group_layouts: &[&compute_bind_group_layout],
        push_constant_ranges: &[],
    });

    // create render pipeline with empty bind group layout

    let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("render"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&render_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &draw_shader,
            entry_point: "main_vs",
            compilation_options: Default::default(),
            buffers: &[
                wgpu::VertexBufferLayout {
                    array_stride: 4 * 4,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                },
                wgpu::VertexBufferLayout {
                    array_stride: 2 * 4,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![2 => Float32x2],
                },
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &draw_shader,
            entry_point: "main_fs",
            compilation_options: Default::default(),
            targets: &[Some(surface_config.view_formats[0].into())],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // create compute pipeline

    let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute pipeline"),
        layout: Some(&compute_pipeline_layout),
        module: &compute_shader,
        entry_point: "main",
        compilation_options: Default::default(),
    });

    // buffer for the three 2d triangle vertices of each instance

    let vertex_buffer_data = [-0.01f32, -0.02, 0.01, -0.02, 0.00, 0.02];
    let vertices_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Vertex Buffer"),
        contents: bytemuck::bytes_of(&vertex_buffer_data),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    });

    // buffer for all particles data of type [(posx,posy,velx,vely),...]

    let mut initial_particle_data = vec![0.0f32; (4 * NUM_PARTICLES) as usize];
    let mut rng = WyRand::new_seed(42);
    let mut unif = || rng.generate::<f32>() * 2f32 - 1f32; // Generate a num (-1, 1)
    for particle_instance_chunk in initial_particle_data.chunks_mut(4) {
        particle_instance_chunk[0] = unif(); // posx
        particle_instance_chunk[1] = unif(); // posy
        particle_instance_chunk[2] = unif() * 0.1; // velx
        particle_instance_chunk[3] = unif() * 0.1; // vely
    }

    // creates two buffers of particle data each of size NUM_PARTICLES
    // the two buffers alternate as dst and src for each frame

    let mut particle_buffers = Vec::<wgpu::Buffer>::new();
    let mut particle_bind_groups = Vec::<wgpu::BindGroup>::new();
    for i in 0..2 {
        particle_buffers.push(
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("Particle Buffer {i}")),
                contents: bytemuck::cast_slice(&initial_particle_data),
                usage: wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST,
            }),
        );
    }

    // create two bind groups, one for each buffer as the src
    // where the alternate buffer is used as the dst

    for i in 0..2 {
        particle_bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &compute_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: sim_param_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: particle_buffers[i].as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: particle_buffers[(i + 1) % 2].as_entire_binding(), // bind to opposite buffer
                },
            ],
            label: None,
        }));
    }

    // calculates number of work groups from PARTICLES_PER_GROUP constant
    let work_group_count = ((NUM_PARTICLES as f32) / (PARTICLES_PER_GROUP as f32)).ceil() as u32;

    let mut frame_num = 0usize;

    loop {
        // Wait for the window to require a redraw.
        window.wait_redraw_requested().await;

        frame_num += 1;
        let frame = match surface.get_current_texture() {
            Ok(frame) => frame,
            // If we timed out, just try again
            Err(wgpu::SurfaceError::Timeout) => surface
                .get_current_texture()
                .expect("Failed to acquire next surface texture!"),
            Err(
                // If the surface is outdated, or was lost, reconfigure it.
                wgpu::SurfaceError::Outdated
                | wgpu::SurfaceError::Lost
                // If OutOfMemory happens, reconfiguring may not help, but we might as well try
                | wgpu::SurfaceError::OutOfMemory,
            ) => {
                surface.configure(&device, &surface_config);
                surface
                    .get_current_texture()
                    .expect("Failed to acquire next surface texture!")
            }
        };
        let view = &frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(surface_config.view_formats[0]),
            ..wgpu::TextureViewDescriptor::default()
        });
        // Render code here.
        // create render pass descriptor and its color attachments
        let color_attachments = [Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                // Not clearing here in order to test wgpu's zero texture initialization on a surface texture.
                // Users should avoid loading uninitialized memory since this can cause additional overhead.
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })];
        let render_pass_descriptor = wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &color_attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        };

        // get command encoder
        let mut command_encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        command_encoder.push_debug_group("compute boid movement");
        {
            // compute pass
            let mut cpass = command_encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            cpass.set_pipeline(&compute_pipeline);
            cpass.set_bind_group(0, &particle_bind_groups[frame_num % 2], &[]);
            cpass.dispatch_workgroups(work_group_count, 1, 1);
        }
        command_encoder.pop_debug_group();

        command_encoder.push_debug_group("render boids");
        {
            // render pass
            let mut rpass = command_encoder.begin_render_pass(&render_pass_descriptor);
            rpass.set_pipeline(&render_pipeline);
            // render dst particles
            rpass.set_vertex_buffer(0, particle_buffers[(frame_num + 1) % 2].slice(..));
            // the three instance-local vertices
            rpass.set_vertex_buffer(1, vertices_buffer.slice(..));
            rpass.draw(0..3, 0..NUM_PARTICLES);
        }
        command_encoder.pop_debug_group();

        // update frame count
        frame_num += 1;

        // done
        queue.submit(Some(command_encoder.finish()));
    }
}
