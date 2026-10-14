fn main() {
    println!("cargo:rerun-if-env-changed=MICROGENTIC_RELEASE_REVISION");
    // Release packaging supplies the checked-out revision. Development builds
    // retain the existing live-checkout provenance path.
    if let Ok(revision) = std::env::var("MICROGENTIC_RELEASE_REVISION") {
        assert!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "MICROGENTIC_RELEASE_REVISION must be a full Git commit hash"
        );
        println!("cargo:rustc-env=MICROGENTIC_RELEASE_REVISION={revision}");
    }
}
