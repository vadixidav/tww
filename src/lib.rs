use core::future::Future;
use snafu::{ResultExt, Snafu};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, oneshot, OnceCell};
use winit::{
    application::ApplicationHandler,
    error::{EventLoopError, OsError},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::WindowAttributes,
};

const COMMAND_CHANNEL_DEPTH: usize = 64;

static RUNTIME_CONTEXT: OnceCell<RuntimeContext> = OnceCell::const_new();

#[derive(Clone, Debug, Snafu)]
pub enum TwwError {
    #[snafu(display("OS error: {source}"))]
    Os { source: Arc<OsError> },
    #[snafu(display("winit event loop closed"))]
    EventLoopClosed,
    #[snafu(display("tww runtime closed"))]
    TwwRuntimeClosed,
    #[snafu(display("tww window closed"))]
    TwwWindowClosed,
    #[snafu(display("winit event loop error: {source}"))]
    WinitEventLoop { source: Arc<EventLoopError> },
}

pub type Result<T> = core::result::Result<T, TwwError>;

/// This function does not return until the window closes and all resources are closed unless an error occurs during runtime.
///
/// Pass in `main`, which will be spawned on the `tokio` runtime and act as your entry point.
pub fn start<F>(wgpu_instance_descriptor: wgpu::InstanceDescriptor, main: F) -> Result<()>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
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
            runtime_commander: runtime_commander.clone(),
            winit_commander: winit_commander.clone(),
        })
        .ok()
        .expect("only call tww::start once");

    rt.spawn(runtime(runtime_commands, winit_commander));
    rt.spawn(main);

    let mut app = Application {
        instance: wgpu::Instance::new(wgpu_instance_descriptor),
        runtime_commander,
    };

    event_loop
        .run_app(&mut app)
        .map_err(Arc::new)
        .context(WinitEventLoopSnafu)
}

async fn runtime(
    mut commands: mpsc::Receiver<RuntimeCommand>,
    _winit_commander: EventLoopProxy<WinitCommand>,
) {
    let mut windows = HashMap::new();
    while let Some(command) = commands.recv().await {
        match command {
            RuntimeCommand::RegisterWindow { responder, window } => {
                let (commander, commands) = mpsc::channel(COMMAND_CHANNEL_DEPTH);
                windows.insert(window.id(), commander.downgrade());
                tokio::spawn(WindowWorker { commands, window }.task());
                // We can ignore the error if the user has dropped their future.
                responder.send(Ok(Window { commander })).ok();
            }
            RuntimeCommand::WindowClosed { window_id, result } => {
                // Ignore send errors since the worker terminates if it is dropped.
                if let Some(window_commander) = windows
                    .get(&window_id)
                    .expect("window cannot be closed if it was not registered")
                    .upgrade()
                {
                    // If we can upgrade the sender then the window object still exists and we can inform it.
                    window_commander
                        .send(WindowCommand::ConfirmClosed { result })
                        .await
                        .ok();
                }
                windows.remove(&window_id);
            }
        }
    }
}

enum WinitCommand {
    CreateWindow {
        responder: oneshot::Sender<Result<Window>>,
        attributes: WindowAttributes,
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
}

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. Call [`Window::close`] to asynchronously wait on window closure.
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
}

impl Window {
    pub async fn new() -> Result<Self> {
        Self::with_attributes(WindowAttributes::default()).await
    }

    pub async fn with_attributes(attributes: WindowAttributes) -> Result<Self> {
        let (responder, response) = oneshot::channel();
        context()
            .winit_commander
            .send_event(WinitCommand::CreateWindow {
                responder,
                attributes,
            })
            .map_err(|_| TwwError::EventLoopClosed)?;
        response.await.map_err(|_| TwwError::TwwRuntimeClosed)?
    }

    fn new_impl(
        responder: oneshot::Sender<Result<Window>>,
        runtime_commander: &mpsc::Sender<RuntimeCommand>,
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
                runtime_commander
                    .blocking_send(RuntimeCommand::RegisterWindow { responder, window })
                    .expect("winit event loop running inside of async context");
            }
            Err(e) => {
                // We can ignore the error if the user has dropped their future.
                responder.send(Err(e)).ok();
            }
        }
    }

    /// Request that the window be closed and wait for it to close.
    ///
    /// Returns any errors related to closing.
    pub async fn close(&self) -> Result<()> {
        self.close_request().await?;
        self.wait_close().await
    }

    /// Request that the window be closed without waiting.
    ///
    /// Note that since you aren't waiting for the window to actually close that you won't receive any
    /// errors related to winit's closing process.
    ///
    /// Returns an error only if the event loop is closed.
    pub async fn close_request(&self) -> Result<()> {
        self.commander
            .send(WindowCommand::Close)
            .await
            .map_err(|_| TwwError::EventLoopClosed)
    }

    /// Wait for the window to close without requesting it to close.
    pub async fn wait_close(&self) -> Result<()> {
        let (responder, response) = oneshot::channel();
        self.commander
            .send(WindowCommand::WaitClose { responder })
            .await
            .map_err(|_| TwwError::EventLoopClosed)?;
        response.await.map_err(|_| TwwError::TwwRuntimeClosed)?
    }
}

struct WindowWorker {
    commands: mpsc::Receiver<WindowCommand>,
    window: Arc<winit::window::Window>,
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
}

struct Application {
    #[allow(dead_code)]
    instance: wgpu::Instance,
    runtime_commander: mpsc::Sender<RuntimeCommand>,
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
                self.runtime_commander
                    .blocking_send(RuntimeCommand::WindowClosed {
                        window_id,
                        result: Ok(()),
                    })
                    .expect("winit event loop running inside of async context");
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
                Window::new_impl(responder, &self.runtime_commander, event_loop, attributes);
            }
        }
    }
}

struct RuntimeContext {
    #[allow(dead_code)]
    runtime_commander: mpsc::Sender<RuntimeCommand>,
    winit_commander: EventLoopProxy<WinitCommand>,
}

fn context() -> &'static RuntimeContext {
    RUNTIME_CONTEXT
        .get()
        .expect("you must call tww::start to run your application")
}
