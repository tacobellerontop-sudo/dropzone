//! Engine behind Dropzone: every operation the desktop app offers lives here so it
//! can be tested without a window, and so the UI layer stays a thin shell.

pub mod archive;
pub mod checksum;
pub mod fsops;
pub mod image_ops;
pub mod jobs;
pub mod media;
pub mod ops;
pub mod pdf;
pub mod rename;
pub mod util;

pub use ops::{Operation, OperationOutput};
pub use util::{Cancel, Progress};
