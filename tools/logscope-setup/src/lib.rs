//! Core of the LogScope offline graphical extractor (ADR-0018).
//!
//! Exposed as a library so the extractor binary and the build-time
//! `append-payload` utility share **one** definition of the payload trailer.
//! A second implementation of that layout — for example in a packaging
//! script — could drift and produce a setup executable the stub refuses to
//! read, which is exactly the class of defect that shipped unnoticed from
//! v0.0 to 0.2.1.

pub mod extract;
pub mod payload;
