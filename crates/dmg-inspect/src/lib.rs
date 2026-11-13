//! Bounded, verified read-only PostgreSQL catalog inspection.
#![forbid(unsafe_code)]

mod catalog;
mod history;
mod transport;

pub use catalog::{CatalogDetails, CatalogError, CatalogOptions};
pub use history::{HistoryError, HistoryOptions};
pub use transport::{
    InspectionTarget, InspectionTimeouts, ServerInfo, TransportError, TransportStage,
};
