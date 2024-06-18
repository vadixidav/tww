mod runtime;
mod tegui;
mod twinit;
mod window;

use futures::FutureExt;
use runtime::{RuntimeCommand, RuntimeWorker};
use snafu::{ResultExt, Snafu};
use std::{collections::HashMap, fmt::Debug, future::Future, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch, OnceCell};
use twinit::{Application, WinitCommand};
pub use window::Window;
use window::WindowCommand;
pub use winit::window::WindowAttributes;
use winit::{
    error::{EventLoopError, OsError},
    event_loop::{EventLoop, EventLoopProxy},
};

const COMMAND_CHANNEL_DEPTH: usize = 64;

static RUNTIME_CONTEXT: OnceCell<TwwContext> = OnceCell::const_new();

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
pub fn start<F, T, E>(
    instance: impl Into<Arc<wgpu::Instance>>,
    main: impl FnOnce(Arc<wgpu::Instance>) -> F + Send + 'static,
) -> FinishResult<T, E>
where
    F: Future<Output = CoreResult<T, E>> + Send + 'static,
    F::Output: Send + 'static,
    T: Debug + 'static,
    E: std::error::Error + 'static,
{
    let instance = instance.into();
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
        .set(TwwContext {
            runtime_commander,
            winit_commander,
            lifecycle: watch::Sender::new(LifecycleStage::Background),
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
    rt.spawn({
        let instance = instance.clone();
        async move {
            // Ignore error here because if winit event loop closes randomly we still return the receiver
            // but the user may choose to throw it away and discard it.
            return_tx.send(main(instance).await).ok();
        }
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

pub struct LifecycleWatcher {
    subscription: watch::Receiver<LifecycleStage>,
}

impl LifecycleWatcher {
    /// Creates a new [`tww::LifecycleWatcher`]`.
    pub fn new() -> Self {
        let mut subscription = context().lifecycle.subscribe();
        // Force the consumer to retrieve the next matching lifecycle event. This is important because
        // changes can happen prior to the application starting.
        subscription.mark_changed();
        Self { subscription }
    }

    /// The current [`tww::LifecycleStage`] of the application.
    ///
    /// Calling this marks the last stage transition as seen for the purposes of other methods.
    /// If you are trying to determine if the lifecycle changed or entered some state, use the other methods.
    pub fn lifecycle(&mut self) -> LifecycleStage {
        *self.subscription.borrow_and_update()
    }

    /// Returns any [`tww::LifecycleStage`] that has been set which was not previously seen by this watcher.
    ///
    /// This function guarantees that you will be notified even if the same [`tww::LifecycleStage`] is set twice.
    /// This is important since every time [`tww::LifecycleStage::Rendering`] is entered, you must
    /// recreate your rendering resources, such as surfaces.
    ///
    /// See [`tww::LifecycleStage`] for more details.
    pub async fn wait_lifecycle(&mut self) -> LifecycleStage {
        self.subscription
            .changed()
            .await
            .expect("tww::context cannot be destroyed");
        *self.subscription.borrow_and_update()
    }

    /// Returns a [`tww::LifecycleStage`] if it was not previously seen by this watcher.
    ///
    /// This function guarantees that you will be notified even if the same [`tww::LifecycleStage`] is set twice.
    /// This is important since every time [`tww::LifecycleStage::Rendering`] is entered, you must
    /// recreate your rendering resources, such as surfaces.
    ///
    /// See [`tww::LifecycleStage`] for more details.
    pub fn entered_lifecycle(&mut self) -> Option<LifecycleStage> {
        self.subscription
            .changed()
            .now_or_never()?
            .expect("tww::context cannot be destroyed");
        Some(*self.subscription.borrow_and_update())
    }

    /// Returns when the specified [`tww::LifecycleStage`] is entered.
    ///
    /// This function guarantees that you will be notified if the requested stage is updated before you begin
    /// listening to avoid missing any stage updates. This is important since every time
    /// [`tww::LifecycleStage::Rendering`] is entered, you must recreate your rendering resources, such as surfaces.
    ///
    /// See [`tww::LifecycleStage`] for more details.
    pub async fn wait_stage(&mut self, stage: LifecycleStage) {
        self.subscription
            .wait_for(move |&new_stage| new_stage == stage)
            .await
            .expect("tww::context cannot be destroyed");
    }

    /// Returns `true` if the specified [`tww::LifecycleStage`] is entered.
    ///
    /// This function guarantees that you will be notified if the requested stage is updated before you begin
    /// listening to avoid missing any stage updates. This is important since every time
    /// [`tww::LifecycleStage::Rendering`] is entered, you must recreate your rendering resources, such as surfaces.
    ///
    /// See [`tww::LifecycleStage`] for more details.
    pub fn entered_stage(&mut self, stage: LifecycleStage) -> bool {
        self.subscription
            .wait_for(move |&new_stage| new_stage == stage)
            .now_or_never()
            .map(|r| r.expect("tww::context cannot be destroyed"))
            .is_some()
    }

    /// Waits for the application to enter the rendering stage.
    ///
    /// This is shorthand for `self.wait_stage(LifecycleStage::Rendering)`.
    pub async fn wait_rendering(&mut self) {
        self.wait_stage(LifecycleStage::Rendering).await;
    }

    /// Returns `true` if the application entered the rendering stage.
    ///
    /// This is shorthand for `self.entered_stage(LifecycleStage::Rendering)`.
    pub fn entered_rendering(&mut self) -> bool {
        self.entered_stage(LifecycleStage::Rendering)
    }

    /// Waits for the application to enter the background stage.
    ///
    /// This is shorthand for `self.wait_stage(LifecycleStage::Background)`.
    pub async fn wait_background(&mut self) {
        self.wait_stage(LifecycleStage::Background).await;
    }

    /// Returns `true` if the application entered the background stage.
    ///
    /// This is shorthand for `self.endered_stage(LifecycleStage::Background)`.
    pub fn entered_background(&mut self) -> bool {
        self.entered_stage(LifecycleStage::Background)
    }
}

/// Describes the current stage of the application lifecycle your application is currently in.
///
/// ## Rendering
///
/// In this stage you may create new windows, get window surfaces, and broadly use graphics libraries
/// to perform rendering. Whenever this stage is entered for any reason, any surfaces must be re-created
/// using [`tww::Window::create_surface`]. The async/await APIs provided to retrieve the lifecycle will notify you
/// if you get two back-to-back rendering lifecycle updates, which can happen if you miss a background update.
///
/// ## Background
///
/// In this stage you should cease rendering, as the results are not currently being displayed. It is also recommended
/// to prepare in case the application may be shut down by saving any data with the assumption that your application
/// could be terminated at any time without any warning by the platform. Certain platforms may broadly guarantee
/// under typical conditions that your application will go into this background stage before being closed, but
/// this is not true on all platforms. If you intend to support all platforms, you should ensure data is always saved.
///
/// Note that being in the background stage of the lifecycle does not mean that you are minimized. A minimized
/// application is still expected to render frames regardless.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifecycleStage {
    Rendering,
    Background,
}

struct TwwContext {
    runtime_commander: mpsc::Sender<RuntimeCommand>,
    winit_commander: EventLoopProxy<WinitCommand>,
    lifecycle: watch::Sender<LifecycleStage>,
}

impl TwwContext {
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

    fn window_command(&self, window_id: winit::window::WindowId, command: WindowCommand) {
        self.runtime_command(RuntimeCommand::WindowCommand { window_id, command });
    }
}

fn context() -> &'static TwwContext {
    RUNTIME_CONTEXT
        .get()
        .expect("you must call tww::start to run your application")
}

#[doc(hidden)]
pub fn start_test(test: impl Future<Output = CoreResult<(), TwwError>> + Send + 'static) {
    start::<_, (), TwwError>(
        wgpu::Instance::new(wgpu::InstanceDescriptor::default()),
        move |_| test,
    )
    .ok();
}
