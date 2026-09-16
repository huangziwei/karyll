//! Makes the build stamp reach the binary. `BUILD` in `main.rs` reads
//! `KARYLL_BUILD` through `option_env!`, which cargo cannot see, so without this
//! the crate looks fresh when only the stamp has changed.

fn main() {
    println!("cargo:rerun-if-env-changed=KARYLL_BUILD");
}
