//! Safe Rust API over the FreeRDP 3 client core.

pub mod framebuffer;
pub mod keymap;
pub mod session;

pub use framebuffer::{FrameBuffer, Rect};
pub use session::{
    AuthReason, Button, CertVerdict, CertificateInfo, Config, Credentials, Cursor, Error, Handler,
    Monitor, RailWindow, Session, SessionHandle, State, error_message, rail, validate,
};
