//! Bounded, verified transport for read-only PostgreSQL inspection.
#![forbid(unsafe_code)]

mod transport;

pub use transport::{
    InspectionTarget, InspectionTimeouts, ServerInfo, TransportError, TransportStage,
};
