//! Connection profiles: model, storage, FreeRDP argument generation and
//! import/export (.rdp files, Remmina).

pub mod args;
pub mod model;
pub mod rdpfile;
pub mod remmina;
pub mod store;

pub use args::{ConnectParams, redact, to_args};
pub use model::*;
pub use store::Store;
