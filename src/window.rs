use snafu::ResultExt;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::WindowAttributes};

use crate::{context, OsSnafu, Result, RuntimeCommand, WinitCommand};

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. Call [`Window::close`] to asynchronously wait on window closure.
#[derive(Clone)]
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
    window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
}

impl Window {
    pub async fn new() -> Result<Self> {
        Self::with_attributes(WindowAttributes::default()).await
    }

    pub async fn with_attributes(attributes: WindowAttributes) -> Result<Self> {
        let (responder, response) = oneshot::channel();
        context().winit_command(WinitCommand::CreateWindow {
            responder,
            attributes,
        });
        response
            .await
            .expect("winit event loop or tww::runtime closed unexpectedly")
    }

    pub(crate) fn new_winit(
        responder: oneshot::Sender<Result<Window>>,
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) {
        let window = event_loop
            .create_window(attributes)
            .map_err(Arc::new)
            .context(OsSnafu)
            .map(Arc::new);
        match window {
            Ok(window) => {
                context().runtime_command(RuntimeCommand::RegisterWindow { responder, window });
            }
            Err(e) => {
                // We can ignore the error if the user has dropped their future.
                responder.send(Err(e)).ok();
            }
        }
    }

    pub(crate) fn new_runtime(
        commander: mpsc::Sender<WindowCommand>,
        commands: mpsc::Receiver<WindowCommand>,
        window: Arc<winit::window::Window>,
    ) -> Window {
        let redraw_requested = Arc::new(Notify::new());
        tokio::spawn(
            WindowWorker {
                commands,
                window: window.clone(),
                redraw_requested: redraw_requested.clone(),
            }
            .task(),
        );
        // We can ignore the error if the user has dropped their future.
        Window {
            redraw_requested,
            window,
            commander,
        }
    }

    async fn command(&self, command: WindowCommand) {
        self.commander
            .send(command)
            .await
            .expect("tww::WindowWorker not accepting commands");
    }

    /// Close the window and wait for it to close.
    ///
    /// This is equivalent to:
    /// ```no_run
    /// # tww::start_test(async {
    /// # let window = tww::Window::new().await?;
    /// window.close_request().await;
    /// window.wait_close().await;
    /// # Ok(())
    /// # });
    /// ```
    ///
    /// Returns any errors related to closing.
    pub async fn close(&self) -> Result<()> {
        self.close_request().await;
        self.wait_close().await
    }

    /// Request that the window be closed.
    ///
    /// This does not wait for the window to be closed, only requests that it be closed.
    pub async fn close_request(&self) {
        self.command(WindowCommand::Close).await;
    }

    /// Wait for the window to close.
    ///
    /// This does not request that the window be closed, only waits for it to be closed.
    ///
    /// Returns any errors related to closing.
    pub async fn wait_close(&self) -> Result<()> {
        let (responder, response) = oneshot::channel();
        self.command(WindowCommand::WaitClose { responder }).await;
        response.await.expect("tww::runtime closed unexpectedly")
    }

    /// Creates a [`wgpu::Surface`] for this window.
    pub async fn create_surface(&self) -> Result<wgpu::Surface> {
        let (responder, response) = oneshot::channel();
        self.command(WindowCommand::CreateSurface { responder })
            .await;
        response
            .await
            .expect("winit event loop closed unexpectedly")
    }

    /// Wait until a redraw is requested for this window.
    pub async fn redraw_requested(&self) {
        self.redraw_requested.notified().await;
    }

    /// Get the pixel dimension of the inside of the window.
    ///
    /// See [`winit::window::Window::inner_size`].
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        self.window.inner_size()
    }
}

pub(crate) enum WindowCommand {
    Close,
    WaitClose {
        responder: oneshot::Sender<Result<()>>,
    },
    ConfirmClosed {
        result: Result<()>,
    },
    CreateSurface {
        responder: oneshot::Sender<Result<wgpu::Surface<'static>>>,
    },
    RedrawRequested,
}

struct WindowWorker {
    commands: mpsc::Receiver<WindowCommand>,
    window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
}

impl WindowWorker {
    async fn task(mut self) {
        let mut close_responders = Vec::new();
        let mut close_result = None;
        while let Some(command) = self.commands.recv().await {
            match command {
                WindowCommand::Close => break,
                WindowCommand::WaitClose { responder } => {
                    close_responders.push(responder);
                }
                WindowCommand::ConfirmClosed { result } => {
                    close_result = Some(result.clone());
                    for responder in close_responders.drain(..) {
                        // We can ignore the error if the user has dropped their future.
                        responder.send(result.clone()).ok();
                    }
                    break;
                }
                WindowCommand::CreateSurface { responder } => {
                    context().winit_command(WinitCommand::CreateWindowSurface {
                        responder,
                        window: self.window.clone(),
                    });
                }
                WindowCommand::RedrawRequested => {
                    self.redraw_requested.notify_one();
                }
            }
        }

        // Dropping this window handle closes the window, but not right away.
        // We get an event from winit when the window has actually closed.
        drop(self.window);

        while let Some(command) = self.commands.recv().await {
            match command {
                WindowCommand::WaitClose { responder } => {
                    if let Some(close_result) = close_result.clone() {
                        // We can ignore the error if the user has dropped their future.
                        responder.send(close_result).ok();
                    } else {
                        close_responders.push(responder);
                    }
                }
                WindowCommand::ConfirmClosed { result } => {
                    close_result = Some(result.clone());
                    for responder in close_responders.drain(..) {
                        // We can ignore the error if the user has dropped their future.
                        responder.send(result.clone()).ok();
                    }
                }
                _ => {
                    // There is nothing to do for commands that don't require a response since
                    // the window is gone.
                }
            }
        }
    }
}
