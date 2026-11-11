//! Boucle de fond : actualisation automatique des données, rappel quotidien de ce qu'il reste
//! à faire et vérification des mises à jour.

use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate};
use tauri::{AppHandle, Manager, Runtime};

use crate::rewards::FetchError;
use crate::store::Settings;
use crate::AppState;

pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let logged_in = crate::refresh(&app).await.is_ok();
        refresh_xbox(&app).await;
        // Formule Game Pass relue une fois par semaine, discrètement.
        if logged_in {
            let before = app.state::<AppState>().store.settings().subscription;
            crate::gamepass::detect_weekly(&app).await;
            let settings = app.state::<AppState>().store.settings();
            if settings.subscription != before {
                let _ = tauri::Emitter::emit(&app, "settings-updated", &settings);
            }
        }
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
            maybe_level_alert(&app, &settings);
            maybe_weekly_summary(&app, &settings);
            maybe_birthday_reminder(&app, &settings);
        }
    });
}

/// Quêtes Xbox, seulement si un compte Xbox Live est connecté.
async fn refresh_xbox<R: Runtime>(app: &AppHandle<R>) {
    if crate::xbox::status(app).connected {
        let _ = crate::xbox::refresh(app).await;
        crate::xbox::sync_played_days(app).await;
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

/// Dernier jour du mois de `day`.
fn month_end(day: NaiveDate) -> NaiveDate {
    let (y, m) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap() - chrono::Duration::days(1)
}

/// Dans les trois derniers jours du mois, après l'heure du rappel : si les points du mois ne
/// suffisent pas à garder le niveau actuel, une alerte (une par mois).
fn maybe_level_alert<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    if !settings.notify_level {
        return;
    }
    let now = Local::now();
    let today = now.date_naive();
    let end = month_end(today);
    if (end - today).num_days() > 2 || now.time() < settings.reminder_time() {
        return;
    }
    let state = app.state::<AppState>();
    let Some(status) = state
        .summary
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.level_status.clone())
    else {
        return;
    };
    let Some(level) = status.levels.iter().find(|l| l.level == status.current) else {
        return;
    };
    let missing = level.points - status.month_points;
    if level.points == 0 || missing <= 0 {
        return;
    }
    if !state
        .store
        .first_alert(&format!("niveau#{}", today.format("%Y-%m")))
    {
        return;
    }
    let name = level
        .title
        .clone()
        .unwrap_or_else(|| format!("niveau {}", level.level));
    let days = (end - today).num_days() + 1;
    crate::notify_with(
        app,
        &format!("Niveau {name} en jeu"),
        &format!(
            "Encore {missing} points d'ici la fin du mois pour le garder ({} / {} pts, {days} jour{} restant{}).",
            status.month_points,
            level.points,
            if days > 1 { "s" } else { "" },
            if days > 1 { "s" } else { "" },
        ),
        crate::ClickAction::OpenMain(Some("today")),
    );
}

/// Le dimanche soir, après 20 h : points gagnés dans la semaine et comparaison avec la
/// précédente, séries et niveau. Une fois par semaine.
fn maybe_weekly_summary<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    if !settings.weekly_summary {
        return;
    }
    let now = Local::now();
    let today = now.date_naive();
    if today.weekday() != chrono::Weekday::Sun
        || now.time() < chrono::NaiveTime::from_hms_opt(20, 0, 0).unwrap()
    {
        return;
    }
    let state = app.state::<AppState>();
    let week = today.iso_week();
    if !state
        .store
        .first_alert(&format!("bilan#{}-W{:02}", week.year(), week.week()))
    {
        return;
    }
    let monday = today - chrono::Duration::days(6);
    let gained = state.store.points_gained(monday, today);
    let before = state.store.points_gained(
        monday - chrono::Duration::days(7),
        monday - chrono::Duration::days(1),
    );
    let mut lines = vec![if before > 0 {
        let diff = gained - before;
        let trend = match diff {
            0 => "autant que la semaine précédente".to_string(),
            d if d > 0 => format!("{d} de plus que la semaine précédente"),
            d => format!("{} de moins que la semaine précédente", -d),
        };
        format!("+{gained} points cette semaine, {trend}.")
    } else {
        format!("+{gained} points cette semaine.")
    }];
    if let Some(summary) = state.summary.lock().unwrap().as_ref() {
        if let Some(status) = &summary.level_status {
            if let Some(next) = status.levels.iter().find(|l| l.level == status.current + 1) {
                lines.push(format!(
                    "Ce mois-ci : {} / {} pts pour le niveau {}.",
                    status.month_points,
                    next.points,
                    next.title.clone().unwrap_or_default()
                ));
            }
        }
        let pending: Vec<_> = summary
            .streaks
            .streaks
            .iter()
            .filter(|s| !s.today_done)
            .map(|s| s.title.clone())
            .collect();
        if !pending.is_empty() {
            lines.push(format!("À faire avant minuit : {}.", pending.join(", ")));
        }
    }
    crate::notify_with(
        app,
        "Votre semaine Microsoft Rewards",
        &lines.join("\n"),
        crate::ClickAction::OpenMain(Some("history")),
    );
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
            None => lines.extend(xbox_manual_lines(app, settings)),
        }
    }
    (lines, complete)
}

/// Quêtes Xbox du jour pas encore faites, d'après le suivi (appareils et formule choisis).
fn xbox_manual_lines<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Vec<String> {
    let xbox = app.state::<AppState>().store.xbox_progress();
    let game_pass = matches!(
        settings.subscription,
        crate::store::Subscription::Premium | crate::store::Subscription::Ultimate
    );
    let mut lines = Vec::new();
    if settings.xbox_pc && !xbox.today.pc {
        lines.push(format!(
            "Xbox : jouer 15 min sur PC (bonus hebdomadaire {}/5 jours)",
            xbox.pc_days
        ));
    }
    if settings.xbox_console && !xbox.today.console {
        lines.push(format!(
            "Xbox : jouer 15 min sur console (bonus hebdomadaire {}/5 jours)",
            xbox.console_days
        ));
    }
    if settings.xbox_mobile && !xbox.today.mobile_app {
        lines.push(format!(
            "Xbox : se connecter à l'application mobile (série de {} jour{})",
            xbox.mobile_streak,
            if xbox.mobile_streak > 1 { "s" } else { "" }
        ));
    }
    if settings.xbox_mobile && !xbox.today.jewel {
        lines.push("Xbox : terminer un niveau de Jewel dans l'application mobile".into());
    }
    if game_pass && !xbox.today.game_pass {
        lines.push(format!(
            "Xbox : jouer 15 min à un jeu Game Pass (série {}/5 jours)",
            xbox.game_pass_days
        ));
    }
    lines
}

/// Semaine d'anniversaire (lundi à dimanche) : l'offre est à consulter dans les Rewards Xbox,
/// et le cadeau se réclame la semaine suivante. Un rappel au début de chacune des deux.
fn maybe_birthday_reminder<R: Runtime>(app: &AppHandle<R>, settings: &Settings) {
    let today = Local::now().date_naive();
    let Ok(birthday) = NaiveDate::parse_from_str(
        &format!("{}-{}", today.year(), settings.birthday.trim()),
        "%Y-%m-%d",
    ) else {
        return;
    };
    let monday =
        |d: NaiveDate| d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64);
    let week = monday(birthday);
    let state = app.state::<AppState>();
    if monday(today) == week {
        if state
            .store
            .first_alert(&format!("anniversaire#{}", today.year()))
        {
            crate::notify_with(
                app,
                "Semaine d'anniversaire Xbox Rewards",
                "Ouvrez le Rewards Hub de l'app Xbox pour voir votre offre d'anniversaire. Cette semaine, les jeux et extensions achetés dans le Store rapportent le double de points.",
                crate::ClickAction::OpenMain(Some("today")),
            );
        }
    } else if monday(today) == week + chrono::Duration::days(7)
        && state
            .store
            .first_alert(&format!("cadeau-anniversaire#{}", today.year()))
    {
        crate::notify_with(
            app,
            "Cadeau d'anniversaire à réclamer",
            "Votre cadeau d'anniversaire Xbox Rewards se réclame cette semaine dans le Rewards Hub de l'app Xbox.",
            crate::ClickAction::OpenMain(Some("today")),
        );
    }
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
