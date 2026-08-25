fn main() {
    tonic_prost_build::compile_protos("proto/nemr.proto")
        .expect("failed to compile proto/nemr.proto");
    println!("cargo:rerun-if-changed=proto/nemr.proto");
}
