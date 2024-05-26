use core::fmt::Debug;
use core::future::Future;
use snafu::{ResultExt, Snafu};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{
    mpsc::{self, WeakSender},
    oneshot, Notify, OnceCell,
};
use winit::{
    application::ApplicationHandler,
    error::{EventLoopError, OsError},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::WindowAttributes,
};

const COMMAND_CHANNEL_DEPTH: usize = 64;

static RUNTIME_CONTEXT: OnceCell<RuntimeContext> = OnceCell::const_new();

#[derive(Debug, Snafu)]
pub enum TwwFinishError<T, E>
where
    E: std::error::Error + 'static,
{
    #[snafu(display("tww error: {source}"))]
    Tww { source: TwwError },
    #[snafu(display("winit event loop error: {source}"))]
    WinitEventLoop {
        source: Arc<EventLoopError>,
        main_result: oneshot::Receiver<CoreResult<T, E>>,
    },
    #[snafu(display("user main error: {source}"))]
    UserMainError { source: E },
    #[snafu(display("user main panic"))]
    UserMainPanic,
}

#[derive(Clone, Debug, Snafu)]
pub enum TwwError {
    #[snafu(display("OS error: {source}"))]
    Os { source: Arc<OsError> },
    #[snafu(display("tww window closed"))]
    TwwWindowClosed,
    #[snafu(display("create window surface error: {source}"))]
    CreateWindowSurface { source: wgpu::CreateSurfaceError },
}

pub type CoreResult<T, E> = core::result::Result<T, E>;
pub type Result<T> = CoreResult<T, TwwError>;
pub type FinishResult<T, E> = CoreResult<T, TwwFinishError<T, E>>;

/// This function does not return until the window closes and all resources are closed unless an error occurs during runtime.
///
/// Pass in `main`, which will be spawned on the `tokio` runtime and act as your entry point.
pub fn start<F, T, E>(instance: wgpu::Instance, main: F) -> FinishResult<T, E>
where
    F: Future<Output = CoreResult<T, E>> + Send + 'static,
    F::Output: Send + 'static,
    T: Debug + 'static,
    E: std::error::Error + 'static,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (runtime_commander, runtime_commands) = mpsc::channel(COMMAND_CHANNEL_DEPTH);

    let event_loop = EventLoop::with_user_event()
        .build()
        .expect("couldn't create a winit event loop");

    let winit_commander = event_loop.create_proxy();

    RUNTIME_CONTEXT
        .set(RuntimeContext {
            runtime_commander,
            winit_commander,
        })
        .ok()
        .expect("only call tww::start once");

    let (return_tx, return_rx) = oneshot::channel();
    rt.spawn(
        RuntimeWorker {
            commands: runtime_commands,
            windows: HashMap::new(),
        }
        .task(),
    );
    rt.spawn(async move {
        // Ignore error here because if winit event loop closes randomly we still return the receiver
        // but the user may choose to throw it away and discard it.
        return_tx.send(main.await).ok();
    });

    let mut app = Application { instance };

    if let Err(e) = event_loop.run_app(&mut app).map_err(Arc::new) {
        return Err(e).context(WinitEventLoopSnafu {
            main_result: return_rx,
        });
    }

    return_rx
        .blocking_recv()
        .map_err(|_| TwwFinishError::UserMainPanic)?
        .context(UserMainSnafu)
}

struct RuntimeWorker {
    commands: mpsc::Receiver<RuntimeCommand>,
    windows: HashMap<winit::window::WindowId, WeakSender<WindowCommand>>,
}

impl RuntimeWorker {
    async fn task(mut self) {
        while let Some(command) = self.commands.recv().await {
            match command {
                RuntimeCommand::RegisterWindow { responder, window } => {
                    let (commander, commands) = mpsc::channel(COMMAND_CHANNEL_DEPTH);
                    self.windows.insert(window.id(), commander.downgrade());
                    // We can ignore the error if the user has dropped their future.
                    responder
                        .send(Ok(Window::new_runtime(commander, commands, window)))
                        .ok();
                }
                RuntimeCommand::WindowClosed { window_id, result } => {
                    self.window_command(window_id, WindowCommand::ConfirmClosed { result })
                        .await;
                    self.windows.remove(&window_id);
                }
                RuntimeCommand::WindowRedrawRequested { window_id } => {
                    self.window_command(window_id, WindowCommand::RedrawRequested)
                        .await;
                }
            }
        }
    }

    async fn window_command(&self, id: winit::window::WindowId, command: WindowCommand) {
        // If we can upgrade the sender then the window object still exists and we can inform it.
        // If we can't upgrade the sender then the window object is gone so we can throw away the command
        // as the window will be closed by winit shortly and then cleaned up by the runtime.
        if let Some(window_commander) = self
            .windows
            .get(&id)
            .expect("winit generated event for unknown window")
            .upgrade()
        {
            // Ignore send errors since the worker terminates if it is dropped.
            window_commander.send(command).await.ok();
        }
    }
}

enum WinitCommand {
    CreateWindow {
        responder: oneshot::Sender<Result<Window>>,
        attributes: WindowAttributes,
    },
    CreateWindowSurface {
        responder: oneshot::Sender<Result<wgpu::Surface<'static>>>,
        window: Arc<winit::window::Window>,
    },
}

enum RuntimeCommand {
    RegisterWindow {
        responder: oneshot::Sender<Result<Window>>,
        window: Arc<winit::window::Window>,
    },
    WindowClosed {
        window_id: winit::window::WindowId,
        result: Result<()>,
    },
    WindowRedrawRequested {
        window_id: winit::window::WindowId,
    },
}

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. Call [`Window::close`] to asynchronously wait on window closure.
#[derive(Clone)]
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
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
        response.await.expect("tww::runtime closed unexpectedly")
    }

    fn new_winit(
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

    fn new_runtime(
        commander: mpsc::Sender<WindowCommand>,
        commands: mpsc::Receiver<WindowCommand>,
        window: Arc<winit::window::Window>,
    ) -> Window {
        let redraw_requested = Arc::new(Notify::new());
        tokio::spawn(
            WindowWorker {
                commands,
                window,
                redraw_requested: redraw_requested.clone(),
            }
            .task(),
        );
        // We can ignore the error if the user has dropped their future.
        Window {
            redraw_requested,
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
    /// # tww::start::<_, (), tww::TwwError>(wgpu::Instance::new(wgpu::InstanceDescriptor::default()), async {
    /// # let window = tww::Window::new().await?;
    /// window.close_request().await;
    /// window.wait_close().await;
    /// # }).ok();
    /// # Ok(())
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

enum WindowCommand {
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

struct Application {
    instance: wgpu::Instance,
}

impl ApplicationHandler<WinitCommand> for Application {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        todo!()
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        todo!()
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::Destroyed => {
                context().runtime_command(RuntimeCommand::WindowClosed {
                    window_id,
                    result: Ok(()),
                });
            }
            WindowEvent::RedrawRequested => {
                context().runtime_command(RuntimeCommand::WindowRedrawRequested { window_id })
            }
            _ => {}
        }
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
        }
    }
}

struct RuntimeContext {
    runtime_commander: mpsc::Sender<RuntimeCommand>,
    winit_commander: EventLoopProxy<WinitCommand>,
}

impl RuntimeContext {
    fn runtime_command(&self, command: RuntimeCommand) {
        // TODO: Have two separated expected() statements, one for each case, for clarity.
        self.runtime_commander.blocking_send(command).expect(
            "winit event loop running inside of async context or tww::runtime closed unexpectedly",
        );
    }

    fn winit_command(&self, command: WinitCommand) {
        self.winit_commander
            .send_event(command)
            .ok()
            .expect("winit event loop closed unexpectedly");
    }
}

fn context() -> &'static RuntimeContext {
    RUNTIME_CONTEXT
        .get()
        .expect("you must call tww::start to run your application")
}
