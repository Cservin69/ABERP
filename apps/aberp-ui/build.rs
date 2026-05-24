// Tauri 2 build-script entry. Parses `tauri.conf.json` +
// `capabilities/*.json` and emits the runtime wire-format.
fn main() {
    tauri_build::build();
}
