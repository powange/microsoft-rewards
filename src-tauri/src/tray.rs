//! Icône de la zone de notification : l'app y reste quand la fenêtre est fermée.

use tauri::image::Image;
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
                    crate::rewards::forget_quest_pages();
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
        // Clic gauche : mini-fenêtre ; double-clic : fenêtre principale.
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } => crate::mini::toggle(tray.app_handle(), position),
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                crate::mini::hide(tray.app_handle());
                crate::show_main(tray.app_handle());
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Non connecté : infobulle de base et icône sans pastille.
pub fn set_points<R: Runtime>(app: &AppHandle<R>, points: Option<i64>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let text = match points {
            Some(points) => format!("Rewards Autoplay · {} points", group_thousands(points)),
            None => "Rewards Autoplay · non connecté".to_string(),
        };
        let _ = tray.set_tooltip(Some(text));
        if let Some(icon) = app.default_window_icon() {
            let _ = tray.set_icon(Some(icon.clone()));
        }
    }
}

/// Infobulle « 12 480 points · 4 tâches à faire », et une pastille orange sur l'icône tant
/// qu'une série n'a pas son étape du jour.
pub fn set_status<R: Runtime>(
    app: &AppHandle<R>,
    points: i64,
    tasks_left: usize,
    streak_pending: bool,
) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let mut text = format!("Rewards Autoplay · {} points", group_thousands(points));
    match tasks_left {
        0 => text.push_str(" · tout est fait"),
        1 => text.push_str(" · 1 tâche à faire"),
        n => text.push_str(&format!(" · {n} tâches à faire")),
    }
    if streak_pending {
        text.push_str(" · série à faire");
    }
    let _ = tray.set_tooltip(Some(text));
    if let Some(icon) = app.default_window_icon() {
        let icon = if streak_pending {
            with_badge(icon)
        } else {
            icon.clone()
        };
        let _ = tray.set_icon(Some(icon));
    }
}

/// Copie de l'icône avec une pastille orange cerclée de blanc, en bas à droite.
fn with_badge(icon: &Image<'_>) -> Image<'static> {
    let (w, h) = (icon.width() as i64, icon.height() as i64);
    let mut rgba = icon.rgba().to_vec();
    let r = (w.min(h) as f64 * 0.26).max(2.0);
    let (cx, cy) = (w as f64 - r - 0.5, h as f64 - r - 0.5);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            let color: Option<[u8; 4]> = if d <= r - r * 0.28 {
                Some([0xF0, 0x8A, 0x24, 0xFF])
            } else if d <= r {
                Some([0xFF, 0xFF, 0xFF, 0xFF])
            } else {
                None
            };
            if let Some(c) = color {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&c);
            }
        }
    }
    Image::new_owned(rgba, w as u32, h as u32)
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

    #[test]
    fn pastille_en_bas_a_droite() {
        let icon = tauri::image::Image::new_owned(vec![0; 32 * 32 * 4], 32, 32);
        let badged = super::with_badge(&icon);
        let px =
            |x: usize, y: usize| badged.rgba()[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].to_vec();
        assert_eq!(px(25, 25), vec![0xF0, 0x8A, 0x24, 0xFF]);
        assert_eq!(px(2, 2), vec![0, 0, 0, 0]);
    }
}
