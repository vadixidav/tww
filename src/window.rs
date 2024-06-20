use crate::{
    context, tegui::EguiRenderer, twinit::WinitCommand, LifecycleWatcher, OsSnafu, Result,
    RuntimeCommand, TwwError, WindowAttributes, COMMAND_CHANNEL_DEPTH,
};
use futures::FutureExt;
use raw_window_handle::{HasDisplayHandle, HasRawDisplayHandle, HasWindowHandle};
use snafu::ResultExt;
use std::sync::Arc;
use tokio::sync::{broadcast, futures::Notified, mpsc, oneshot, watch, Notify};
use winit::{
    dpi::{LogicalSize, PhysicalSize},
    event::KeyEvent,
    event_loop::ActiveEventLoop,
};

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. Call [`Window::close`] to asynchronously wait on window closure.
#[derive(Clone)]
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
    pub(crate) window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
    dimensions: watch::Sender<PhysicalSize<u32>>,
    close_requested: watch::Sender<bool>,
    keyboard_events: broadcast::Sender<KeyEvent>,
    window_events: broadcast::Sender<winit::event::WindowEvent>,
}

impl Window {
    /// Opens a window.
    ///
    /// This will wait until entering the rendering lifecycle stage before opening the window.
    pub async fn new() -> Result<Self> {
        LifecycleWatcher::new()
            .wait_stage(crate::LifecycleStage::Rendering)
            .await;
        Self::new_ignore_lifecycle().await
    }

    /// Opens a window assuming we are in the rendering lifecycle stage.
    pub async fn new_ignore_lifecycle() -> Result<Self> {
        Self::with_attributes(WindowAttributes::default()).await
    }

    /// Opens a window assuming we are in the rendering lifecycle stage with specific attributes.
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
        let close_requested = watch::Sender::new(false);
        let keyboard_events = broadcast::Sender::new(COMMAND_CHANNEL_DEPTH);
        let window_events = broadcast::Sender::new(COMMAND_CHANNEL_DEPTH);

        tokio::spawn(
            WindowWorker {
                commands,
                window: window.clone(),
                redraw_requested: redraw_requested.clone(),
                dimensions: dimensions.clone(),
                close_requested: close_requested.clone(),
                keyboard_events: keyboard_events.clone(),
                window_events: window_events.clone(),
            }
            .task(),
        );

        Window {
            redraw_requested,
            window,
            commander,
            dimensions,
            close_requested,
            keyboard_events,
            window_events,
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

    /// Create an [`EguiRenderer`].
    ///
    /// This lets you render frames with egui onto the window surface.
    pub fn create_egui_renderer(
        &self,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        target_format: wgpu::TextureFormat,
        clear_color: wgpu::Color,
        depth_format: Option<wgpu::TextureFormat>,
    ) -> EguiRenderer {
        EguiRenderer::new(
            self,
            device,
            queue,
            target_format,
            clear_color,
            depth_format,
        )
    }

    /// Ask the window to redraw and wait until it is ready to be redrawn.
    pub fn redraw(&self) -> Notified<'_> {
        self.window.request_redraw();
        self.wait_redraw_requested()
    }

    /// Ask the window to redraw.
    ///
    /// This doesn't mean the window is ready for you to redraw yet, which you can wait on with [`wait_redraw_requested`].
    pub fn redraw_no_wait(&self) {
        self.window.request_redraw();
    }

    /// Wait until a redraw is requested for this window.
    pub fn wait_redraw_requested(&self) -> Notified<'_> {
        self.redraw_requested.notified()
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

    /// Get the size of the inside of the window.
    ///
    /// See [`winit::window::Window::inner_size`].
    pub fn size(&self) -> LogicalSize<u32> {
        self.window
            .inner_size()
            .to_logical(self.window.scale_factor())
    }

    /// Watch for changes to window dimensions.
    pub fn dimensions_watcher(&self) -> WindowDimensionsWatcher {
        let mut subscription = self.dimensions.subscribe();
        // Force the consumer to retrieve the currently set size. This is important because
        // changes can happen prior to the application starting.
        subscription.mark_changed();
        WindowDimensionsWatcher { subscription }
    }

    /// Waits until a close is requested.
    pub async fn wait_close_requested(&self) {
        let mut subscription = self.close_requested.subscribe();
        subscription.wait_for(|&c| c).await.ok();
    }

    /// Listen to window keyboard events.
    pub fn keyboard_listener(&self) -> WindowKeyboardListener {
        WindowKeyboardListener {
            subscription: self.keyboard_events.subscribe(),
        }
    }

    /// Listen to all window events.
    ///
    /// It is not recommended to use this directly and to instead use listeners for the data you are interested in.
    /// This is useful for implementing support for things which directly tie into winit.
    pub fn window_listener(&self) -> WindowEventListener {
        WindowEventListener {
            subscription: self.window_events.subscribe(),
        }
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        self.window.display_handle()
    }
}

impl HasWindowHandle for Window {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        self.window.window_handle()
    }
}

pub(crate) enum WindowCommand {
    Close,
    WaitClose {
        responder: oneshot::Sender<Result<()>>,
    },
    CreateSurface {
        responder: oneshot::Sender<Result<wgpu::Surface<'static>>>,
    },
    WinitWindowEvent {
        event: winit::event::WindowEvent,
    },
}

struct WindowWorker {
    commands: mpsc::Receiver<WindowCommand>,
    window: Arc<winit::window::Window>,
    redraw_requested: Arc<Notify>,
    dimensions: watch::Sender<PhysicalSize<u32>>,
    close_requested: watch::Sender<bool>,
    keyboard_events: broadcast::Sender<KeyEvent>,
    window_events: broadcast::Sender<winit::event::WindowEvent>,
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
                WindowCommand::CreateSurface { responder } => {
                    context().winit_command(WinitCommand::CreateWindowSurface {
                        responder,
                        window: self.window.clone(),
                    });
                }
                WindowCommand::WinitWindowEvent { event } => {
                    use winit::event::WindowEvent;
                    match event.clone() {
                        WindowEvent::Destroyed => {
                            close_result = Some(Ok(()));
                            for responder in close_responders.drain(..) {
                                // We can ignore the error if the user has dropped their future.
                                responder.send(Ok(())).ok();
                            }
                            break;
                        }
                        WindowEvent::RedrawRequested => {
                            self.redraw_requested.notify_one();
                        }
                        WindowEvent::Resized(dimensions) => {
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
                        WindowEvent::KeyboardInput { event, .. } => {
                            self.keyboard_events.send(event).ok();
                        }
                        WindowEvent::CloseRequested => {
                            self.close_requested.send_replace(true);
                        }
                        _ => {}
                    }
                    self.window_events.send(event).ok();
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
                WindowCommand::WinitWindowEvent {
                    event: winit::event::WindowEvent::Destroyed,
                } => {
                    close_result = Some(Ok(()));
                    for responder in close_responders.drain(..) {
                        // We can ignore the error if the user has dropped their future.
                        responder.send(Ok(())).ok();
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

pub struct WindowKeyboardListener {
    subscription: broadcast::Receiver<KeyEvent>,
}

impl WindowKeyboardListener {
    /// Returns a [`winit::event::KeyEvent`] if a new one is available, consuming it.
    pub fn event(&mut self) -> Option<KeyEvent> {
        self.subscription.try_recv().ok()
    }

    /// Waits for a [`winit::event::KeyEvent`].
    pub async fn wait_event(&mut self) -> KeyEvent {
        loop {
            if let Some(event) = self.subscription.recv().await.ok() {
                return event;
            }
        }
    }
}

pub struct WindowEventListener {
    subscription: broadcast::Receiver<winit::event::WindowEvent>,
}

impl WindowEventListener {
    /// Returns a [`winit::event::WindowEvent`] if a new one is available, consuming it.
    pub fn event(&mut self) -> Option<winit::event::WindowEvent> {
        self.subscription.try_recv().ok()
    }

    /// Waits for a [`winit::event::WindowEvent`].
    pub async fn wait_event(&mut self) -> winit::event::WindowEvent {
        loop {
            if let Some(event) = self.subscription.recv().await.ok() {
                return event;
            }
        }
    }
}
