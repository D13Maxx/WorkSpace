fn main() {
    // The Understand-Anything pipeline is a bundled resource, built by
    // scripts/build-ua-pipeline.mjs and gitignored. Tauri refuses to build when
    // a resource is missing, so a fresh clone or a CI check gets an empty
    // folder: the app then just offers no AI graph. Release builds the real one.
    let _ = std::fs::create_dir_all("ua-pipeline");
    tauri_build::build();
}
