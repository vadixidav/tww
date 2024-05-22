# tww (Tokio + WebGPU + Winit)

This is a highly opinionated library that you use when you want to use `tokio`, `wgpu`, and `winit`. You interact with `winit` exclusively through async/await so that you never have to worry about event loops again. It will automatically take care of recreating the window surface and rendering resources at the correct times based on the application lifecycle so that you don't need to worry about these details. You provide what capabilities you need to do your rendering and it will take care of the rest.
