#[path = "../../pg.rs"]
mod engine;
#[path = "../../probe.rs"]
mod probe;

fn pg_parse(sql: &str) -> pg_query::Result<pg_query::ParseResult> {
    pg_query::parse(sql, pg_query::ParserOptions::default())
}

fn main() {
    probe::run("pg_query 18.1.0", Some(180006));
}
