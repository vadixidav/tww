use crate::{context, twinit::WinitCommand, OsSnafu, Result, RuntimeCommand, TwwError};
use futures::FutureExt;
use snafu::ResultExt;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::WindowAttributes};

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. Call [`Window::close`] to asynchronously wait on window closure.
#[derive(Clone)]
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
    window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
    dimensions: watch::Sender<PhysicalSize<u32>>,
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
        let dimensions = watch::Sender::new(window.inner_size());

        tokio::spawn(
            WindowWorker {
                commands,
                window: window.clone(),
                redraw_requested: redraw_requested.clone(),
                dimensions: dimensions.clone(),
            }
            .task(),
        );

        Window {
            redraw_requested,
            window,
            commander,
            dimensions,
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
    /// window.close_no_wait().await;
    /// window.wait_close().await;
    /// # Ok(())
    /// # });
    /// ```
    ///
    /// Returns any errors related to closing.
    pub async fn close(&self) -> Result<()> {
        self.close_no_wait().await;
        self.wait_close().await
    }

    /// Request that the window be closed.
    ///
    /// This does not wait for the window to be closed, only requests that it be closed.
    pub async fn close_no_wait(&self) {
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

    /// Ask the window to redraw and wait until it is ready to be redrawn.
    pub async fn redraw(&self) {
        self.window.request_redraw();
        self.wait_redraw_requested().await;
    }

    /// Ask the window to redraw.
    ///
    /// This doesn't mean the window is ready for you to redraw yet, which you can wait on with [`wait_redraw_requested`].
    pub fn redraw_no_wait(&self) {
        self.window.request_redraw();
    }

    /// Wait until a redraw is requested for this window.
    pub async fn wait_redraw_requested(&self) {
        self.redraw_requested.notified().await;
    }

    /// Check if a redraw is requested for this window.
    ///
    /// If this returns `true` it wont return `true` again until another redraw is requested.
    pub fn is_redraw_requested(&self) -> bool {
        self.redraw_requested.notified().now_or_never().is_some()
    }

    /// Get the pixel dimension of the inside of the window.
    ///
    /// See [`winit::window::Window::inner_size`].
    pub fn dimensions(&self) -> PhysicalSize<u32> {
        self.window.inner_size()
    }

    /// Get a watcher to listen for changes to window dimensions.
    pub fn dimensions_watcher(&self) -> WindowDimensionsWatcher {
        let mut subscription = self.dimensions.subscribe();
        // Force the consumer to retrieve the currently set size. This is important because
        // changes can happen prior to the application starting.
        subscription.mark_changed();
        WindowDimensionsWatcher { subscription }
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
    UpdateDimensions {
        dimensions: PhysicalSize<u32>,
    },
}

struct WindowWorker {
    commands: mpsc::Receiver<WindowCommand>,
    window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
    dimensions: watch::Sender<PhysicalSize<u32>>,
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
                WindowCommand::UpdateDimensions { dimensions } => {
                    // Dimensions updates where the dimensions are unchanged are spurious.
                    self.dimensions.send_if_modified(|current_dimensions| {
                        if *current_dimensions != dimensions {
                            *current_dimensions = dimensions;
                            true
                        } else {
                            false
                        }
                    });
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

pub struct WindowDimensionsWatcher {
    subscription: watch::Receiver<PhysicalSize<u32>>,
}

impl WindowDimensionsWatcher {
    /// The current inner dimensions of the window.
    ///
    /// Calling this marks the current dimensions as seen for the purposes of other methods.
    /// If you are trying to determine if the dimensions changed to update your textures, use the other methods.
    pub fn dimensions(&mut self) -> PhysicalSize<u32> {
        *self.subscription.borrow_and_update()
    }

    /// Returns any change to the inner dimensions of the window.
    ///
    /// [`tww::TwwError::TwwWindowClosed`] is returned if the window has been closed.
    pub async fn wait_dimensions(&mut self) -> Result<PhysicalSize<u32>> {
        self.subscription
            .changed()
            .await
            .map_err(|_| TwwError::TwwWindowClosed)?;
        Ok(*self.subscription.borrow_and_update())
    }

    /// Returns updated window dimensions if they have changed.
    ///
    /// [`tww::TwwError::TwwWindowClosed`] is returned if the window has been closed.
    pub fn changed_dimensions(&mut self) -> Result<Option<PhysicalSize<u32>>> {
        Ok(self
            .subscription
            .changed()
            .now_or_never()
            .transpose()
            .map_err(|_| TwwError::TwwWindowClosed)?
            .map(|_| *self.subscription.borrow_and_update()))
    }
}
