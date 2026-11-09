// Pas de console en plus de la fenêtre en version release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rewards_autoplay_lib::run()
}
