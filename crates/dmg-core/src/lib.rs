//! Shared contracts for offline PostgreSQL migration analysis.

pub mod application;
pub mod coverage;
pub mod diff;
pub mod digest;
pub mod history;
pub mod inspection;
pub mod migration;
pub mod policy;
pub mod rehearsal;
pub mod report;
pub mod risk;
pub mod snapshot;
pub mod source;
pub mod target;

mod schema;
