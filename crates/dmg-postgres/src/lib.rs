//! Typed PostgreSQL migration facts and original source spans.
#![forbid(unsafe_code)]

mod expression;
mod flyway;
mod normalize;
mod operation;
mod parse;
mod version;

pub use operation::*;
pub use parse::{parse_migration, parse_plan};
