//! Icône de la zone de notification : l'app y reste quand la fenêtre est fermée.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Runtime};

const TRAY_ID: &str = "main";

pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Ouvrir Rewards Autoplay", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Actualiser les points", true, None::<&str>)?;
    let update = MenuItem::with_id(
        app,
        "update",
        "Rechercher une mise à jour",
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &refresh, &update, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Rewards Autoplay")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => crate::show_main(app),
            "refresh" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::refresh(&app).await;
                });
            }
            "update" => {
                crate::show_main(app);
                let _ = app.emit("check-update-requested", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Affiche le solde de points au survol de l'icône.
pub fn set_points<R: Runtime>(app: &AppHandle<R>, points: Option<i64>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let text = match points {
            Some(points) => format!("Rewards Autoplay · {} points", group_thousands(points)),
            None => "Rewards Autoplay · non connecté".to_string(),
        };
        let _ = tray.set_tooltip(Some(text));
    }
}

fn group_thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn groups_thousands_french_style() {
        assert_eq!(super::group_thousands(12345), "12\u{202f}345");
        assert_eq!(super::group_thousands(999), "999");
        assert_eq!(super::group_thousands(1000000), "1\u{202f}000\u{202f}000");
    }
}
