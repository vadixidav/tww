use core::future::Future;
use snafu::{ResultExt, Snafu};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify, OnceCell};
use winit::{
    application::ApplicationHandler,
    error::EventLoopError,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::WindowAttributes,
};

const COMMAND_CHANNEL_DEPTH: usize = 64;

static RUNTIME_CONTEXT: OnceCell<RuntimeContext> = OnceCell::const_new();

#[derive(Debug, Snafu)]
enum CreateWindowError {
    #[snafu(display("tww failed to open a new window: {source}"))]
    Os { source: winit::error::OsError },
}

/// This function does not return until the window closes and all resources are closed unless an error occurs during runtime.
///
/// Pass in `main`, which will be spawned on the `tokio` runtime and act as your entry point.
pub fn start<F>(main: F) -> Result<(), EventLoopError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _rt_guard = rt.enter();

    let (commander, commands) = mpsc::channel(COMMAND_CHANNEL_DEPTH);

    RUNTIME_CONTEXT
        .set(RuntimeContext { commander })
        .ok()
        .expect("nothing else can initialize tww::RUNTIME_CONTEXT");

    let event_loop = EventLoop::with_user_event()
        .build()
        .expect("couldn't create a winit event loop");

    tokio::spawn(runtime(commands, event_loop.create_proxy()));
    tokio::spawn(main);

    let mut app = Application {};

    event_loop.run_app(&mut app)
}

async fn runtime(
    mut commands: mpsc::Receiver<RuntimeCommand>,
    winit_commander: EventLoopProxy<WinitCommand>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            RuntimeCommand::CreateWindow {
                responder,
                attributes,
            } => {
                winit_commander
                    .send_event(WinitCommand::CreateWindow {
                        responder,
                        attributes,
                    })
                    .ok()
                    .expect("winit event loop should not close unless tww initiates it");
            }
        }
    }
}

enum WinitCommand {
    CreateWindow {
        responder: oneshot::Sender<Result<Window, CreateWindowError>>,
        attributes: WindowAttributes,
    },
}

enum RuntimeCommand {
    CreateWindow {
        responder: oneshot::Sender<Result<Window, CreateWindowError>>,
        attributes: WindowAttributes,
    },
}

/// This wraps an operating system window.
///
/// When dropped, the window closes with eventual consistency. To close
pub struct Window {
    commander: mpsc::Sender<WindowCommand>,
}

impl Window {
    fn new(
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, CreateWindowError> {
        let _window = Arc::new(event_loop.create_window(attributes).context(OsSnafu)?);

        let (commander, mut commands) = mpsc::channel(COMMAND_CHANNEL_DEPTH);
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    WindowCommand::Close { finished } => {
                        todo!()
                    }
                }
            }
        });

        Ok(Self { commander })
    }

    pub async fn close(&self) {
        todo!()
    }
}

enum WindowCommand {
    Close { finished: Notify },
}

struct Application {}

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
        _window_id: winit::window::WindowId,
        _event: WindowEvent,
    ) {
        todo!()
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WinitCommand) {
        match event {
            WinitCommand::CreateWindow {
                responder,
                attributes,
            } => {
                responder.send(Window::new(event_loop, attributes)).ok();
            }
        }
    }
}

struct RuntimeContext {
    commander: mpsc::Sender<RuntimeCommand>,
}
