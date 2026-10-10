//! Interchange with other editors: CMX3600 EDL and Final Cut Pro XML (FCPXML).
//!
//! Import is two-step because the core cannot open media files: `parse`
//! returns the media the document references; the caller (the engine) probes
//! and adds them to the project and then asks for the timeline to be built
//! with a resolver from reference to [`crate::id::MediaId`].

pub mod edl;
pub mod fcpxml;
pub(crate) mod xml;

#[derive(Debug, thiserror::Error)]
pub enum InterchangeError {
    #[error("parse error at line {line}: {msg}")]
    Parse { line: usize, msg: String },
    #[error("XML: {0}")]
    Xml(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}
