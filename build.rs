// Rebuild when migrations or templates change: both are embedded into the
// binary at compile time (`sqlx::migrate!` and `include_str!`).
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=static");
}
