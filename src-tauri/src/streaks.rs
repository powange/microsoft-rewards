//! Séries de la page /earn : série Recherche Bing, série quotidienne, série d'applications…
//!
//! Chaque série est un bloc `{"type":"streak"|"dailyset","model":{"streak":{…},"expiresAt":…}}`.
//! Les titres et libellés sont des clés de traduction (`DailyCheckIn_Bing_Title`) dont le
//! texte français figure ailleurs dans la page (`"DailyCheckIn_Bing_Title":"Série Recherche Bing"`).

use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::flight;

const REWARDS_ORIGIN: &str = "https://rewards.bing.com/";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Streak {
    pub id: String,
    pub title: String,
    /// Jours déjà validés dans la série en cours, y compris aujourd'hui s'il est fait.
    pub completed_days: usize,
    pub total_days: usize,
    /// Points gagnés chaque jour de la série ; le dernier jour porte le bonus.
    pub daily_points: Vec<i64>,
    pub today_done: bool,
    /// Avancement du jour, tel qu'affiché par le site (« Recherche : 1/1 »).
    pub today_label: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamps {
    pub collected: i64,
    pub total: i64,
    pub bonus_points: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Streaks {
    pub streaks: Vec<Streak>,
    pub stamps: Option<Stamps>,
    /// Nombre de jours d'affilée de la série en cours.
    pub counter: Option<i64>,
    pub protection_on: Option<bool>,
    pub protection_days_left: Option<i64>,
    /// Date de la prochaine réinitialisation des séries quotidiennes.
    pub reset_at: Option<String>,
}

pub fn parse(text: &str) -> Streaks {
    let mut result = Streaks::default();
    for block in flight::objects_with_key(text, "type") {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(model) = block.get("model").and_then(Value::as_object) else {
            continue;
        };
        match kind {
            "streak" | "dailyset" => {
                let Some(streak) = model.get("streak").and_then(Value::as_object) else {
                    continue;
                };
                if streak.get("isEnabled").and_then(Value::as_bool) == Some(false) {
                    continue;
                }
                if result.reset_at.is_none() {
                    result.reset_at = model
                        .get("expiresAt")
                        .and_then(Value::as_str)
                        .and_then(|d| d.strip_prefix("$D"))
                        .map(str::to_string);
                }
                if let Some(streak) = parse_streak(streak, text) {
                    if !result.streaks.iter().any(|s| s.id == streak.id) {
                        result.streaks.push(streak);
                    }
                }
            }
            "streakbonus" if result.stamps.is_none() => {
                let bonus = &model.get("bonus").cloned().unwrap_or_default();
                if let Some(total) = int(bonus, "activitiesTotal") {
                    result.stamps = Some(Stamps {
                        collected: int(bonus, "activityProgress").unwrap_or(0),
                        total,
                        bonus_points: int(bonus, "streakBonus").unwrap_or(0),
                    });
                }
            }
            "dailystreak" if result.counter.is_none() => {
                result.counter = model.get("streakCounter").and_then(Value::as_i64);
                let protection = model.get("streakProtection");
                result.protection_on = protection
                    .and_then(|p| p.get("isProtectionOn"))
                    .and_then(Value::as_bool);
                result.protection_days_left = protection
                    .and_then(|p| p.get("remainingDays"))
                    .and_then(Value::as_i64);
            }
            _ => {}
        }
    }
    result
}

fn parse_streak(streak: &Map<String, Value>, text: &str) -> Option<Streak> {
    let get_str = |key: &str| {
        streak
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && !s.starts_with('$'))
    };
    let id = get_str("partner")?.to_string();
    let title_key = get_str("title").unwrap_or(&id);
    let total_days = streak.get("totalDays").and_then(Value::as_u64)? as usize;
    let done = int_of(streak, "activitiesCompleted").unwrap_or(0);
    let total = int_of(streak, "activitiesTotal").unwrap_or(0);
    let today_label = match get_str("activitiesLabel").and_then(|key| translation(text, key)) {
        Some(template) => template
            .replace("{complete, number}", &done.to_string())
            .replace("{total, number}", &total.to_string()),
        None => format!("{done}/{total}"),
    };
    let url = get_str("destinationUrl")
        .and_then(|href| Url::parse(REWARDS_ORIGIN).ok()?.join(href).ok())
        .map(|u| u.to_string());
    Some(Streak {
        title: translation(text, title_key).unwrap_or_else(|| title_key.to_string()),
        completed_days: streak
            .get("completedDays")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        total_days,
        daily_points: streak
            .get("dailyPoints")
            .and_then(Value::as_array)
            .map(|points| points.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        today_done: streak.get("isCurrentDayCompleted").and_then(Value::as_bool) == Some(true),
        today_label,
        url,
        image_url: get_str("imageUrl")
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        id,
    })
}

/// Texte français d'une clé de traduction, s'il figure dans la page.
fn translation(text: &str, key: &str) -> Option<String> {
    flight::values_after_key(text, key)
        .into_iter()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
}

fn int(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn int_of(object: &Map<String, Value>, key: &str) -> Option<i64> {
    object.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extrait réduit de la vraie page /earn (novembre 2026).
    const ROWS: &str = r#"3:{"DailyCheckIn_Bing_Title":"Série Recherche Bing","DailyCheckIn_Sapphire_Title":"Série d’applications Bing","Search_ProgressLabel":"Recherche : {complete, number}/{total, number}","CheckIn_ProgressLabel":"Check-in : {complete, number}/{total, number}"}
ae:["$","$La0",null,{"type":"streak","model":{"streak":{"activitiesCompleted":1,"activitiesLabel":"Search_ProgressLabel","activitiesTotal":1,"completedDays":2,"currentDay":1,"dailyPoints":[3,3,3,3,3,3,100],"destinationUrl":"https://www.bing.com/?form=ML2PCO","imageUrl":"https://bing.com/th?id=OMR.Image.BingStreak1.png&pid=Rewards","isCurrentDayCompleted":true,"isEnabled":true,"partner":"bing","title":"DailyCheckIn_Bing_Title","totalDays":7},"expiresAt":"$D2026-11-09T23:00:00.000Z"}}]
af:["$","$La0",null,{"type":"streak","model":{"streak":{"activitiesCompleted":0,"activitiesLabel":"CheckIn_ProgressLabel","activitiesTotal":1,"completedDays":0,"dailyPoints":[5,5,10,10,15,15,50],"destinationUrl":"/dashboard?section=app","isCurrentDayCompleted":false,"isEnabled":true,"partner":"bingapp","title":"DailyCheckIn_Sapphire_Title","totalDays":7},"expiresAt":"$D2026-11-09T23:00:00.000Z"}}]
b0:["$","$La0",null,{"type":"streak","model":{"streak":{"partner":"edge","title":"DailyCheckIn_Edge_Title","totalDays":7,"isEnabled":false}}}]
b1:{"type":"streakbonus","model":{"bonus":{"activitiesTotal":12,"activityProgress":3,"streakBonus":1000}}}
b2:{"type":"dailystreak","model":{"streakCounter":2,"streakProtection":{"isProtectionOn":false,"remainingDays":14}}}
"#;

    #[test]
    fn parses_streaks_stamps_and_counter() {
        let s = parse(ROWS);
        assert_eq!(s.streaks.len(), 2, "la série désactivée est ignorée");

        let bing = &s.streaks[0];
        assert_eq!(bing.title, "Série Recherche Bing");
        assert_eq!((bing.completed_days, bing.total_days), (2, 7));
        assert!(bing.today_done);
        assert_eq!(bing.today_label, "Recherche : 1/1");
        assert_eq!(bing.daily_points.last(), Some(&100));

        let app = &s.streaks[1];
        assert_eq!(app.title, "Série d’applications Bing");
        assert_eq!(app.today_label, "Check-in : 0/1");
        assert!(!app.today_done);
        assert_eq!(
            app.url.as_deref(),
            Some("https://rewards.bing.com/dashboard?section=app")
        );

        let stamps = s.stamps.unwrap();
        assert_eq!(
            (stamps.collected, stamps.total, stamps.bonus_points),
            (3, 12, 1000)
        );
        assert_eq!(s.counter, Some(2));
        assert_eq!(
            (s.protection_on, s.protection_days_left),
            (Some(false), Some(14))
        );
        assert_eq!(s.reset_at.as_deref(), Some("2026-11-09T23:00:00.000Z"));
    }
}
