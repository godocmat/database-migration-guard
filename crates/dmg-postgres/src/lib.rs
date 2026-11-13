//! Typed PostgreSQL migration facts and original source spans.
#![forbid(unsafe_code)]

mod expression;
mod normalize;
mod operation;
mod parse;
mod supabase;
mod version;

pub use operation::*;
pub use parse::{parse_migration, parse_plan};
