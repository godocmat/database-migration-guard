mod engine;
#[path = "../../probe.rs"]
mod probe;

fn main() {
    probe::run("sqlparser 0.64.0 (std)", None);
}
