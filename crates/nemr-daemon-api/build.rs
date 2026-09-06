// The proto stays at the repository root (`proto/nemr.proto`): it is the
// daemon's public interface, and the installed-engine freshness gate scans
// that path. This crate compiles it in place.
fn main() {
    tonic_prost_build::compile_protos("../../proto/nemr.proto")
        .expect("failed to compile proto/nemr.proto");
    println!("cargo:rerun-if-changed=../../proto/nemr.proto");
}
