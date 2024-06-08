#[cfg(all(feature = "backend-windows", feature = "backend-async-task"))]
compile_error!("only one `backend-*` feature can be selected at a time");

#[cfg(feature = "backend-windows")]
#[path = "window.rs"]
mod _backend;

#[cfg(feature = "backend-async-task")]
#[path = "async_task.rs"]
mod _backend;

pub use _backend::*;
