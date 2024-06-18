use crate::window::{Window, WindowCommand};
use crate::LifecycleStage;
use snafu::ResultExt;
use std::sync::Arc;
use tokio::sync::oneshot;
use winit::{
    application::ApplicationHandler, event::WindowEvent, event_loop::ActiveEventLoop,
    window::WindowAttributes,
};

use crate::{context, CreateWindowSurfaceSnafu, Result};

pub(crate) enum WinitCommand {
    CreateWindow {
        responder: oneshot::Sender<Result<Window>>,
        attributes: WindowAttributes,
    },
    CreateWindowSurface {
        responder: oneshot::Sender<Result<wgpu::Surface<'static>>>,
        window: Arc<winit::window::Window>,
    },
    Terminate,
}

pub(crate) struct Application {
    pub(crate) instance: Arc<wgpu::Instance>,
}

impl ApplicationHandler<WinitCommand> for Application {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        context().lifecycle.send_replace(LifecycleStage::Rendering);
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        context().lifecycle.send_replace(LifecycleStage::Background);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        context().window_command(
            window_id,
            WindowCommand::WinitWindowEvent {
                event: event.clone(),
            },
        );
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WinitCommand) {
        match event {
            WinitCommand::CreateWindow {
                responder,
                attributes,
            } => {
                // We can ignore the error if the user has dropped their future.
                Window::new_winit(responder, event_loop, attributes);
            }
            WinitCommand::CreateWindowSurface { responder, window } => {
                let surface = self
                    .instance
                    .create_surface(window.clone())
                    .context(CreateWindowSurfaceSnafu);
                responder.send(surface).ok();
            }
            WinitCommand::Terminate => {
                event_loop.exit();
            }
        }
    }
}
