// A GUI app: no console window next to it on Windows (debug builds keep one
// for logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    void_app_lib::run();
}
