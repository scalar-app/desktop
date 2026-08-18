// The desktop binary. Every line of behaviour is in the library, which the mobile targets build
// directly, so the platforms cannot drift apart.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    scalar_desktop_lib::run()
}
