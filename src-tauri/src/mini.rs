//! Mini-fenêtre ouverte d'un clic gauche sur l'icône de la zone de notification : solde,
//! tâches du jour et séries, sans ouvrir la fenêtre principale.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};

pub const LABEL: &str = "mini";
const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 520.0;

/// Moment où la mini-fenêtre a été cachée parce qu'elle a perdu le focus. Un clic sur l'icône
/// lui retire d'abord le focus : sans cette mémoire, ce clic la rouvrirait aussitôt au lieu
/// de la fermer.
static HIDDEN_BY_BLUR: Mutex<Option<Instant>> = Mutex::new(None);

/// Crée la fenêtre, cachée. Elle est créée au démarrage : la créer pendant le clic sur
/// l'icône risquerait de bloquer WebView2.
pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("mini.html".into()))
        .title("Rewards Autoplay")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .build()?;
    let handle = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            if handle.is_visible().unwrap_or(false) {
                let _ = handle.hide();
                *HIDDEN_BY_BLUR.lock().unwrap() = Some(Instant::now());
            }
        }
    });
    Ok(())
}

/// Affiche la mini-fenêtre au-dessus de l'icône (ou la cache si elle était ouverte).
pub fn toggle<R: Runtime>(app: &AppHandle<R>, click: PhysicalPosition<f64>) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let just_hidden = HIDDEN_BY_BLUR
        .lock()
        .unwrap()
        .take()
        .is_some_and(|at| at.elapsed() < Duration::from_millis(400));
    if just_hidden || window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    if let (Ok(Some(monitor)), Ok(size)) = (
        app.monitor_from_point(click.x, click.y),
        window.outer_size(),
    ) {
        let area = monitor.work_area();
        let (w, h) = (size.width as i32, size.height as i32);
        let (left, top) = (area.position.x, area.position.y);
        let (right, bottom) = (left + area.size.width as i32, top + area.size.height as i32);
        let margin = 8;
        let x = (click.x as i32 - w / 2).clamp(left + margin, (right - w - margin).max(left));
        // Barre des tâches en bas (cas courant) : au-dessus de l'icône ; sinon en dessous.
        let y = if click.y as i32 > (top + bottom) / 2 {
            bottom - h - margin
        } else {
            top + margin
        };
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let _ = window.emit("mini-shown", ());
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}
