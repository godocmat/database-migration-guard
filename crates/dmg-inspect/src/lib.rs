//! Bounded, verified read-only PostgreSQL catalog inspection.
#![forbid(unsafe_code)]

mod catalog;
mod transport;

pub use catalog::{CatalogDetails, CatalogError, CatalogOptions};
pub use transport::{
    InspectionTarget, InspectionTimeouts, ServerInfo, TransportError, TransportStage,
};
