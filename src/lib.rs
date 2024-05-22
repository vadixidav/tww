use core::future::Future;
use tokio::sync::OnceCell;
use winit::{application::ApplicationHandler, error::EventLoopError, event_loop::EventLoop};

static WINIT_CONTEXT: OnceCell<WinitContext> = OnceCell::const_new();

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

    WINIT_CONTEXT
        .set(WinitContext {})
        .ok()
        .expect("nothing else can initialize WINIT_CONTEXT");

    tokio::spawn(main);

    let mut app = Application {};

    EventLoop::new().unwrap().run_app(&mut app)
}

struct Application {}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        todo!()
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        todo!()
    }
}

struct WinitContext {}
