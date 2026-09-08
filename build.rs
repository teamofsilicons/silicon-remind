//! Rebuild binaries when their embedded production or testing migrations change.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=testing");
}
