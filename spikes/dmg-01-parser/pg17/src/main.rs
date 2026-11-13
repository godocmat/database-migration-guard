#[path = "../../pg.rs"]
mod engine;
#[path = "../../probe.rs"]
mod probe;

fn pg_parse(sql: &str) -> pg_query::Result<pg_query::ParseResult> {
    pg_query::parse(sql)
}

fn main() {
    probe::run("pg_query 6.2.1", Some(170007));
}
