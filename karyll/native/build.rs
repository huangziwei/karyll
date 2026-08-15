//! Makes the build stamp reach the binary.
//!
//! `BUILD` in `main.rs` reads `KARYLL_BUILD` through `option_env!`, which cargo
//! cannot see. Without this the crate is considered fresh when only the stamp
//! has changed, and the binary keeps whichever value was set the last time its
//! source did.

fn main() {
    println!("cargo:rerun-if-env-changed=KARYLL_BUILD");
}
