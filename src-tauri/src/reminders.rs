//! Boucle de fond : actualisation automatique des données, rappel quotidien de ce qu'il reste
//! à faire et vérification des mises à jour.

use std::time::{Duration, Instant};

use chrono::Local;
use tauri::{AppHandle, Manager, Runtime};

use crate::rewards::FetchError;
use crate::store::Settings;
use crate::AppState;

pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let _ = crate::refresh(&app).await;
        refresh_xbox(&app).await;
        let mut last_update_check: Option<Instant> = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            let settings = app.state::<AppState>().store.settings();
            if settings.auto_check_updates
                && last_update_check.is_none_or(|t| t.elapsed() >= settings.update_check_interval())
            {
                last_update_check = Some(Instant::now());
                crate::updater::background_check(&app, settings.auto_install_updates).await;
            }
            if let Some(interval) = settings.refresh_interval() {
                let last = *app.state::<AppState>().last_refresh.lock().unwrap();
                if last.is_none_or(|t| t.elapsed() >= interval) {
                    let _ = crate::refresh(&app).await;
                    refresh_xbox(&app).await;
                }
            }
            maybe_remind(&app, &settings).await;
            maybe_remind_streaks(&app, &settings).await;
        }
    });
}

/// Quêtes Xbox, seulement si un compte Xbox Live est connecté.
async fn refresh_xbox<R: Runtime>(app: &AppHandle<R>) {
    if crate::xbox::status(app).connected {
        let _ = crate::xbox::refresh(app).await;
    }
}

/// Si les données Rewards ne peuvent pas être lues (réseau absent à la sortie de veille…),
/// le rappel est retenté chaque minute pendant ce délai, puis envoyé avec ce qui est connu.
const REMINDER_RETRY: Duration = Duration::from_secs(30 * 60);

/// Un rappel par jour, dès que l'heure choisie est passée (ou au démarrage si elle l'est déjà).
async fn maybe_remind<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    let now = Local::now();
    let today = now.date_naive();
    let state = app.state::<AppState>();
    if state.store.last_reminder() == Some(today) || now.time() < settings.reminder_time() {
        return;
    }
    let (lines, complete) = todo_lines(app, settings).await;
    let late = (now.time() - settings.reminder_time())
        .to_std()
        .is_ok_and(|d| d >= REMINDER_RETRY);
    if !complete && !late {
        log::info!("Rappel reporté : données Rewards illisibles pour le moment");
        return;
    }
    state.store.set_last_reminder(today);
    notify_lines(app, &lines);
}

/// Second rappel, le soir : seulement les séries dont l'étape du jour n'est pas faite, car un
/// jour manqué les remet à zéro. Rien n'est envoyé si toutes sont faites.
async fn maybe_remind_streaks<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    if !settings.remind_streaks_evening {
        return;
    }
    let now = Local::now();
    let today = now.date_naive();
    let state = app.state::<AppState>();
    if state.store.last_streak_reminder() == Some(today)
        || now.time() < settings.streak_reminder_time()
    {
        return;
    }
    let summary = match crate::refresh(app).await {
        Ok(summary) => summary,
        Err(err) => {
            // Réessayé chaque minute jusqu'à minuit ; pas connecté : rien à rappeler.
            if matches!(err, FetchError::NotLoggedIn) {
                state.store.set_last_streak_reminder(today);
            }
            return;
        }
    };
    state.store.set_last_streak_reminder(today);
    let lines: Vec<String> = summary
        .streaks
        .streaks
        .iter()
        .filter(|s| !s.today_done)
        .map(|s| {
            format!(
                "{} : {} ({}/{} jours)",
                s.title, s.today_label, s.completed_days, s.total_days
            )
        })
        .collect();
    if !lines.is_empty() {
        crate::notify_with(
            app,
            "Séries à sauver avant minuit",
            &lines.join("\n"),
            crate::ClickAction::OpenMain(Some("today")),
        );
    }
}

/// Calcule la liste des tâches restantes et l'envoie en notification. Renvoie les lignes envoyées.
pub async fn send<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Vec<String> {
    let (lines, _) = todo_lines(app, settings).await;
    notify_lines(app, &lines);
    lines
}

fn notify_lines<R: Runtime>(app: &AppHandle<R>, lines: &[String]) {
    if !lines.is_empty() {
        crate::notify(app, "À faire aujourd'hui pour Rewards", &lines.join("\n"));
    }
}

/// Lignes du rappel, et `false` si les données Rewards n'ont pas pu être lues.
async fn todo_lines<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut complete = true;
    if settings.remind_bing || settings.remind_streaks {
        match crate::refresh(app).await {
            Ok(summary) => {
                if settings.remind_bing {
                    if summary.claimable_points > 0 {
                        lines.push(format!(
                            "Rewards : {} points prêts à réclamer sur le tableau de bord",
                            summary.claimable_points
                        ));
                    }
                    let points = summary.search_points_left();
                    if points > 0 {
                        lines.push(format!("Bing : {points} points de recherche à gagner"));
                    }
                    match summary.daily_set_left() {
                        0 => {}
                        1 => lines.push("Bing : 1 activité du jour à faire".into()),
                        n => lines.push(format!("Bing : {n} activités du jour à faire")),
                    }
                    match summary.searches_to_do() {
                        0 => {}
                        1 => lines.push("Bing : 1 recherche à faire (activité ou quête)".into()),
                        n => {
                            lines.push(format!("Bing : {n} recherches à faire (activités, quêtes)"))
                        }
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
            Err(err) => {
                complete = false;
                log::warn!("Rappel sans données Rewards : {err}");
            }
        }
    }
    if settings.remind_xbox {
        // Données réelles du Rewards Hub de l'app Xbox si le service répond, sinon le suivi
        // manuel : un compte Xbox Live connecté mais refusé ne doit pas faire perdre le rappel.
        let live = if crate::xbox::status(app).connected {
            crate::xbox::refresh(app).await.ok()
        } else {
            None
        };
        match live {
            Some(rewards) => lines.extend(xbox_lines(&rewards)),
            None => {
                let xbox = app.state::<AppState>().store.xbox_progress();
                if !xbox.today_done {
                    lines.push(format!(
                        "Xbox : jouer 15 min à un jeu Game Pass ({}/{} jours cette semaine)",
                        xbox.week_days, xbox.week_target
                    ));
                }
            }
        }
    }
    (lines, complete)
}

fn xbox_lines(rewards: &crate::xbox::XboxRewards) -> Vec<String> {
    let mut lines = Vec::new();
    for mission in rewards
        .missions
        .iter()
        .filter(|m| m.kind == "single" && m.status == "active")
    {
        lines.push(format!(
            "Xbox : {} (+{} pts)",
            mission.title, mission.points
        ));
    }
    for mission in rewards
        .missions
        .iter()
        .filter(|m| m.kind == "streak" && m.current.is_some())
    {
        lines.push(format!(
            "Xbox : {} ({}/{})",
            mission.title,
            mission.count.unwrap_or(0),
            mission.target.unwrap_or(0)
        ));
    }
    if rewards.unclaimed > 0 {
        lines.push(format!(
            "Xbox : {} points à réclamer dans l'app Xbox",
            rewards.unclaimed
        ));
    }
    lines
}
