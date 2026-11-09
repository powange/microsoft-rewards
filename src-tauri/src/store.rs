//! Réglages et historique local, enregistrés dans un fichier JSON du dossier de données de l'app.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{Datelike, Local, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// L'app ne lance rien : elle envoie un rappel quotidien de ce qu'il reste à faire.
    Reminders,
    /// Lancement automatique des jeux (pas encore disponible, traité comme `Reminders`).
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subscription {
    None,
    Essential,
    Premium,
    Ultimate,
    PcGamePass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub mode: Mode,
    /// Heure du rappel quotidien, au format `HH:MM`.
    pub reminder_time: String,
    pub remind_bing: bool,
    pub remind_xbox: bool,
    pub subscription: Subscription,
    pub auto_check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Reminders,
            reminder_time: "19:00".into(),
            remind_bing: true,
            remind_xbox: true,
            subscription: Subscription::None,
            auto_check_updates: true,
        }
    }
}

impl Settings {
    pub fn reminder_time(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.reminder_time, "%H:%M")
            .unwrap_or_else(|_| NaiveTime::from_hms_opt(19, 0, 0).unwrap())
    }
}

/// Une journée où l'utilisateur a joué au moins 15 minutes à un jeu Game Pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxSession {
    pub game: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    settings: Settings,
    xbox_sessions: BTreeMap<NaiveDate, XboxSession>,
    last_reminder: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxProgress {
    pub today_done: bool,
    pub today_game: Option<String>,
    /// Jours joués dans la semaine en cours (lundi à dimanche).
    pub week_days: u32,
    pub week_target: u32,
    /// Jeux différents joués dans le mois en cours, pour les packs 4 et 8 jeux.
    pub month_games: Vec<String>,
    /// Noms déjà saisis, proposés en suggestion.
    pub known_games: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    data: Mutex<Persisted>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    fn save(&self, data: &Persisted) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(data) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&self.path, text) {
                    log::error!("Impossible d'enregistrer {}: {err}", self.path.display());
                }
            }
            Err(err) => log::error!("Sérialisation de l'état impossible: {err}"),
        }
    }

    pub fn settings(&self) -> Settings {
        self.data.lock().unwrap().settings.clone()
    }

    pub fn set_settings(&self, settings: Settings) {
        let mut data = self.data.lock().unwrap();
        data.settings = settings;
        self.save(&data);
    }

    pub fn last_reminder(&self) -> Option<NaiveDate> {
        self.data.lock().unwrap().last_reminder
    }

    pub fn set_last_reminder(&self, day: NaiveDate) {
        let mut data = self.data.lock().unwrap();
        data.last_reminder = Some(day);
        self.save(&data);
    }

    /// Marque (ou démarque avec `None`) la session Xbox du jour.
    pub fn set_xbox_today(&self, game: Option<String>) {
        let today = Local::now().date_naive();
        let mut data = self.data.lock().unwrap();
        match game {
            Some(game) => {
                data.xbox_sessions.insert(
                    today,
                    XboxSession {
                        game: game.trim().to_string(),
                    },
                );
            }
            None => {
                data.xbox_sessions.remove(&today);
            }
        }
        self.save(&data);
    }

    pub fn xbox_progress(&self) -> XboxProgress {
        let today = Local::now().date_naive();
        let week_start =
            today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        let data = self.data.lock().unwrap();

        let today_game = data.xbox_sessions.get(&today).map(|s| s.game.clone());
        let week_days = data.xbox_sessions.range(week_start..=today).count() as u32;

        let mut seen = BTreeSet::new();
        let mut month_games = Vec::new();
        for (day, session) in &data.xbox_sessions {
            if day.year() == today.year()
                && day.month() == today.month()
                && !session.game.is_empty()
                && seen.insert(session.game.to_lowercase())
            {
                month_games.push(session.game.clone());
            }
        }

        let mut known = BTreeSet::new();
        let known_games = data
            .xbox_sessions
            .values()
            .rev()
            .filter(|s| !s.game.is_empty() && known.insert(s.game.to_lowercase()))
            .map(|s| s.game.clone())
            .collect();

        XboxProgress {
            today_done: today_game.is_some(),
            today_game,
            week_days,
            week_target: 5,
            month_games,
            known_games,
        }
    }
}
