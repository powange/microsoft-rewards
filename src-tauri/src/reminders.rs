//! Boucle de fond : actualisation automatique des données, rappel quotidien de ce qu'il reste
//! à faire et vérification des mises à jour.

use std::time::{Duration, Instant};

use chrono::Local;
use tauri::{AppHandle, Manager, Runtime};

use crate::rewards::FetchError;
use crate::store::Settings;
use crate::AppState;

const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let _ = crate::refresh(&app).await;
        let mut last_update_check: Option<Instant> = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            let settings = app.state::<AppState>().store.settings();
            if settings.auto_check_updates
                && last_update_check.is_none_or(|t| t.elapsed() >= UPDATE_CHECK_EVERY)
            {
                last_update_check = Some(Instant::now());
                crate::updater::background_check(&app).await;
            }
            if let Some(interval) = settings.refresh_interval() {
                let last = *app.state::<AppState>().last_refresh.lock().unwrap();
                if last.is_none_or(|t| t.elapsed() >= interval) {
                    let _ = crate::refresh(&app).await;
                }
            }
            maybe_remind(&app, &settings).await;
        }
    });
}

/// Un rappel par jour, dès que l'heure choisie est passée (ou au démarrage si elle l'est déjà).
async fn maybe_remind<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    let now = Local::now();
    let today = now.date_naive();
    let state = app.state::<AppState>();
    if state.store.last_reminder() == Some(today) || now.time() < settings.reminder_time() {
        return;
    }
    state.store.set_last_reminder(today);
    send(app, settings).await;
}

/// Calcule la liste des tâches restantes et l'envoie en notification. Renvoie les lignes envoyées.
pub async fn send<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Vec<String> {
    let lines = todo_lines(app, settings).await;
    if !lines.is_empty() {
        crate::notify(app, "À faire aujourd'hui pour Rewards", &lines.join("\n"));
    }
    lines
}

async fn todo_lines<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Vec<String> {
    let mut lines = Vec::new();
    if settings.remind_bing || settings.remind_streaks {
        match crate::refresh(app).await {
            Ok(summary) => {
                if settings.remind_bing {
                    let points = summary.search_points_left();
                    if points > 0 {
                        lines.push(format!("Bing : {points} points de recherche à gagner"));
                    }
                    match summary.daily_set_left() {
                        0 => {}
                        1 => lines.push("Bing : 1 activité du jour à faire".into()),
                        n => lines.push(format!("Bing : {n} activités du jour à faire")),
                    }
                }
                if settings.remind_streaks {
                    // Un jour manqué remet la série à zéro : c'est le rappel le plus utile.
                    for streak in summary.streaks.streaks.iter().filter(|s| !s.today_done) {
                        lines.push(format!(
                            "{} : {} ({}/{} jours)",
                            streak.title,
                            streak.today_label,
                            streak.completed_days,
                            streak.total_days
                        ));
                    }
                }
            }
            Err(FetchError::NotLoggedIn) => {
                lines.push("Connectez-vous dans Rewards Autoplay pour suivre vos points".into())
            }
            Err(err) => log::warn!("Rappel sans données Rewards : {err}"),
        }
    }
    if settings.remind_xbox {
        let xbox = app.state::<AppState>().store.xbox_progress();
        if !xbox.today_done {
            lines.push(format!(
                "Xbox : jouer 15 min à un jeu Game Pass ({}/{} jours cette semaine)",
                xbox.week_days, xbox.week_target
            ));
        }
    }
    lines
}
