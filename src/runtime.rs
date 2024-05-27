use crate::Result;
use crate::COMMAND_CHANNEL_DEPTH;
use crate::{window::WindowCommand, Window};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{
    mpsc::{self, WeakSender},
    oneshot,
};

pub enum RuntimeCommand {
    RegisterWindow {
        responder: oneshot::Sender<Result<Window>>,
        window: Arc<winit::window::Window>,
    },
    WindowCommand {
        window_id: winit::window::WindowId,
        command: WindowCommand,
    },
}

pub(crate) struct RuntimeWorker {
    pub(crate) commands: mpsc::Receiver<RuntimeCommand>,
    pub(crate) windows: HashMap<winit::window::WindowId, WeakSender<WindowCommand>>,
}

impl RuntimeWorker {
    pub(crate) async fn task(mut self) {
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
                RuntimeCommand::WindowCommand {
                    window_id,
                    command: command @ WindowCommand::ConfirmClosed { .. },
                } => {
                    self.window_command(window_id, command).await;
                    self.windows.remove(&window_id);
                }
                RuntimeCommand::WindowCommand { window_id, command } => {
                    self.window_command(window_id, command).await;
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
