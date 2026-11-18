//! Reproducible renders from one `.clm`: named static/animated requests share
//! one GPU context/cache while each request owns a fresh continuing Puppet.
//! `spec` normalizes CLI and JSON before GPU work; `frame` also drives GPU-free
//! bounds; `artifacts` defines discoverable output schemas and atomic writes.
//! Selection changes only Part color draws and explicit private mask edges.
//! Neither rendering nor inspection changes the input model file. Animated
//! requests may move a camera over a fixed image size and encode a video.
pub mod args;
pub mod artifacts;
pub mod execute;
pub mod frame;
mod listing;
mod overlay;
pub mod spec;
mod terminal;
pub mod video;
