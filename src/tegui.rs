use ahash::{HashSet, HashSetExt};
use egui::{TexturesDelta, ViewportId};
use egui_winit::ActionRequested;
use std::sync::Arc;
use winit::dpi::PhysicalSize;

use crate::{window::WindowEventListener, Window};

pub struct EguiRenderer {
    window: Window,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    clear_color: wgpu::Color,
    render: egui_wgpu::Renderer,
    window_events: WindowEventListener,
    clipped_primitives: Vec<egui::ClippedPrimitive>,
    winit_state: egui_winit::State,
    textures_delta: TexturesDelta,
}

impl EguiRenderer {
    pub(crate) fn new(
        window: &Window,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        target_format: wgpu::TextureFormat,
        clear_color: wgpu::Color,
        depth_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        let window = window.clone();
        let winit_window = window.window.clone();
        let render = egui_wgpu::Renderer::new(&device, target_format, depth_format, 1);
        let egui_context = egui::Context::default();
        let window_events = window.window_listener();
        let clipped_primitives = vec![];
        let winit_state = egui_winit::State::new(
            egui_context,
            ViewportId::ROOT,
            &winit_window,
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );

        let mut egui_renderer = Self {
            window,
            device,
            queue,
            clear_color,
            render,
            window_events,
            clipped_primitives,
            winit_state,
            textures_delta: TexturesDelta::default(),
        };

        egui_renderer.update_viewport(true);

        egui_renderer
    }

    fn update_viewport(&mut self, init: bool) {
        let mut viewport_info = self.winit_state.egui_input().viewport().clone();
        egui_winit::update_viewport_info(
            &mut viewport_info,
            self.winit_state.egui_ctx(),
            &*self.window.window,
            init,
        );
        self.winit_state
            .egui_input_mut()
            .viewports
            .insert(ViewportId::ROOT, viewport_info);
    }

    pub fn ui(&mut self, run_ui: impl FnOnce(&egui::Context)) {
        while let Some(event) = self.window_events.event() {
            if self
                .winit_state
                .on_window_event(&self.window.window, &event)
                .repaint
            {
                self.window.redraw_no_wait();
            }
        }
        let raw_input = self.winit_state.take_egui_input(&self.window.window);
        let full_output = self.winit_state.egui_ctx().run(raw_input, run_ui);

        // Handle platform output.
        self.winit_state
            .handle_platform_output(&self.window.window, full_output.platform_output);

        // UI output tesselation.
        self.clipped_primitives = self
            .winit_state
            .egui_ctx()
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        // Append texture deltas.
        self.textures_delta.append(full_output.textures_delta);

        // Viewport handling (copy, paste, new windows, title change, etc).
        assert_eq!(full_output.viewport_output.len(), 1);
        let (viewport_id, viewport_output) =
            full_output.viewport_output.into_iter().next().unwrap();
        assert_eq!(viewport_id, ViewportId::ROOT);

        // TODO: Cut, copy, paste, and, screenshot action requests need to be added.
        let mut actions_requested = HashSet::new();
        let mut viewport_info = self.winit_state.egui_input().viewport().clone();
        egui_winit::process_viewport_commands(
            self.winit_state.egui_ctx(),
            &mut viewport_info,
            viewport_output.commands,
            &*self.window.window,
            true,
            &mut actions_requested,
        );
        self.winit_state
            .egui_input_mut()
            .viewports
            .insert(ViewportId::ROOT, viewport_info);

        for action in actions_requested.drain() {
            match action {
                ActionRequested::Screenshot => {
                    // TODO
                }
                ActionRequested::Cut => {
                    self.winit_state
                        .egui_input_mut()
                        .events
                        .push(egui::Event::Cut);
                }
                ActionRequested::Copy => {
                    self.winit_state
                        .egui_input_mut()
                        .events
                        .push(egui::Event::Copy);
                }
                ActionRequested::Paste => {
                    if let Some(contents) = self.winit_state.clipboard_text() {
                        let contents = contents.replace("\r\n", "\n");
                        if !contents.is_empty() {
                            self.winit_state
                                .egui_input_mut()
                                .events
                                .push(egui::Event::Paste(contents));
                        }
                    }
                }
            }
        }

        // self.window.window.set_visible(true);
    }

    pub async fn resized(&mut self, dimensions: PhysicalSize<u32>) {
        let _ = dimensions;
        // egui_winit::update_viewport_info(&mut self.viewport_info, egui_ctx, window, false);
        self.update_viewport(false);
        todo!()
    }

    pub async fn pixels_per_point_changed(&mut self, pixels_per_point: f32) {
        let _ = pixels_per_point;
        // let native_pixels_per_point = *scale_factor as f32;

        //         self.egui_input
        //             .viewports
        //             .entry(self.viewport_id)
        //             .or_default()
        //             .native_pixels_per_point = Some(native_pixels_per_point);
        todo!()
    }

    pub async fn other(&mut self) {
        // self.egui_input.events.push(egui::Event::PointerGone);
        // if !self.has_sent_ime_enabled {
        //     self.egui_input
        //         .events
        //         .push(egui::Event::Ime(egui::ImeEvent::Enabled));
        //     self.has_sent_ime_enabled = true;
        // }
        // self.egui_input
        //                     .events
        //                     .push(egui::Event::Ime(egui::ImeEvent::Preedit(text.clone())));
        // match ime {
        //     winit::event::Ime::Enabled => {}
        //     winit::event::Ime::Preedit(_, None) => {
        //         self.ime_event_enable();
        //     }
        //     winit::event::Ime::Preedit(text, Some(_cursor)) => {
        //         self.ime_event_enable();
        //         self.egui_input
        //             .events
        //             .push(egui::Event::Ime(egui::ImeEvent::Preedit(text.clone())));
        //     }
        //     winit::event::Ime::Commit(text) => {
        //         self.egui_input
        //             .events
        //             .push(egui::Event::Ime(egui::ImeEvent::Commit(text.clone())));
        //         self.ime_event_disable();
        //     }
        //     winit::event::Ime::Disabled => {
        //         self.ime_event_disable();
        //     }
        // };
        // self.on_cursor_moved(window, *position);
        //         EventResponse {
        //             repaint: true,
        //             consumed: self.egui_ctx.is_using_pointer(),
        //         }
        // self.on_mouse_wheel(window, *delta);
        //         EventResponse {
        //             repaint: true,
        //             consumed: self.egui_ctx.wants_pointer_input(),
        //         }
        // self.on_mouse_button_input(*state, *button);
        //         EventResponse {
        //             repaint: true,
        //             consumed: self.egui_ctx.wants_pointer_input(),
        //         }

        // WindowEvent::Touch(touch) => {
        //     self.on_touch(window, touch);
        //     let consumed = match touch.phase {
        //         winit::event::TouchPhase::Started
        //         | winit::event::TouchPhase::Ended
        //         | winit::event::TouchPhase::Cancelled => self.egui_ctx.wants_pointer_input(),
        //         winit::event::TouchPhase::Moved => self.egui_ctx.is_using_pointer(),
        //     };
        todo!()
    }

    pub fn render<'s, 'd>(
        &mut self,
        surface: &wgpu::Surface<'s>,
        depth_stencil_attachment: Option<wgpu::RenderPassDepthStencilAttachment<'d>>,
    ) -> Result<(), wgpu::SurfaceError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tww::tegui::EguiRenderer"),
            });

        // Upload all resources for the GPU.
        let viewport = self.winit_state.egui_input().viewport();
        let dimensions = self.window.dimensions();
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [dimensions.width, dimensions.height],
            pixels_per_point: viewport.native_pixels_per_point.unwrap(),
        };

        let user_cmd_bufs = {
            for (id, image_delta) in &self.textures_delta.set {
                self.render
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }

            self.render.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &self.clipped_primitives,
                &screen_descriptor,
            )
        };

        let output_frame = { surface.get_current_texture()? };

        {
            let view = output_frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            self.render.render(
                &mut render_pass,
                &self.clipped_primitives,
                &screen_descriptor,
            );
        }

        for id in &self.textures_delta.free {
            self.render.free_texture(id);
        }

        let encoded = encoder.finish();

        self.queue
            .submit(user_cmd_bufs.into_iter().chain([encoded]));

        output_frame.present();

        Ok(())
    }
}
