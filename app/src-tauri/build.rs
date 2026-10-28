fn main() {
    // tauri-build embeds icons\icon.ico in the exe but doesn't tell cargo, so a new icon
    // alone would not rerun this script.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
