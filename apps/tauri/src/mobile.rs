//! Mobile entry point for Hrafn Desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    hrafn_desktop::run();
}
