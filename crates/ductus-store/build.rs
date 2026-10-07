// `sqlx::migrate!` embeds the migrations when the crate compiles. Without this, adding a new
// migration file doesn't make cargo rebuild, and the binary silently lacks it.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
