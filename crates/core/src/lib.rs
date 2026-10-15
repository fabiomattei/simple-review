//! UI-independent core of the commit review navigator.

pub mod diff;
pub mod file_view;
pub mod fixture;
pub mod git;
pub mod highlight;
pub mod settings;
pub mod tasks;

pub use gix::ObjectId;
