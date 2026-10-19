//! What's been practised: best scores and time per exercise and per song,
//! and time per day. Kept between runs as RON in eframe's app storage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub exercises: BTreeMap<String, Record>,
    #[serde(default)]
    pub songs: BTreeMap<String, Record>,
    /// Seconds practised each day, by UTC date ("2026-10-08").
    #[serde(default)]
    pub days: BTreeMap<String, f64>,
}

/// One exercise or song.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub seconds: f64,
    /// Finished runs.
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub last: Option<String>,
    /// Best results by variant: a song's mode and hands, or "" for
    /// exercises that have just one.
    #[serde(default)]
    pub bests: BTreeMap<String, Best>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Best {
    /// 0..=1.
    pub score: f64,
    #[serde(default)]
    pub streak: u32,
    /// Average timing error, or time per answer; lower is better.
    #[serde(default)]
    pub timing_ms: Option<f64>,
    pub date: String,
}

/// How a run went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outcome {
    pub score: f64,
    pub streak: u32,
    pub timing_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Exercise,
    Song,
}

impl Progress {
    fn records(&mut self, kind: Kind) -> &mut BTreeMap<String, Record> {
        match kind {
            Kind::Exercise => &mut self.exercises,
            Kind::Song => &mut self.songs,
        }
    }

    /// Adds a finished run. True if it beat the best score so far.
    pub fn finish(
        &mut self,
        kind: Kind,
        name: &str,
        variant: &str,
        outcome: Outcome,
        date: &str,
    ) -> bool {
        let record = self.records(kind).entry(name.to_owned()).or_default();
        record.runs += 1;
        record.last = Some(date.to_owned());
        let new = Best {
            score: outcome.score,
            streak: outcome.streak,
            timing_ms: outcome.timing_ms,
            date: date.to_owned(),
        };
        match record.bests.get_mut(variant) {
            None => {
                record.bests.insert(variant.to_owned(), new);
                true
            }
            Some(best) => {
                let streak = best.streak.max(outcome.streak);
                let better = outcome.score > best.score + 1e-9
                    || ((outcome.score - best.score).abs() <= 1e-9
                        && match (outcome.timing_ms, best.timing_ms) {
                            (Some(a), Some(b)) => a < b,
                            (Some(_), None) => true,
                            _ => false,
                        });
                if better {
                    *best = new;
                }
                best.streak = streak;
                better
            }
        }
    }

    /// Adds practice time to an exercise or song and to the day.
    pub fn practise(&mut self, kind: Kind, name: &str, seconds: f64, date: &str) {
        if seconds <= 0.0 {
            return;
        }
        let record = self.records(kind).entry(name.to_owned()).or_default();
        record.seconds += seconds;
        record.last = Some(date.to_owned());
        *self.days.entry(date.to_owned()).or_default() += seconds;
    }

    pub fn total_seconds(&self) -> f64 {
        self.days.values().sum()
    }

    /// Time practised on each of the `n` days up to `today` (days since
    /// 1970), oldest first.
    pub fn recent_days(&self, today: i64, n: i64) -> Vec<(String, f64)> {
        (today - n + 1..=today)
            .map(|day| {
                let date = date_label(day);
                let seconds = self.days.get(&date).copied().unwrap_or(0.0);
                (date, seconds)
            })
            .collect()
    }

    pub fn to_ron(&self) -> String {
        ron::to_string(self).unwrap_or_default()
    }

    /// What was saved, or a fresh start if it can't be read.
    pub fn from_ron(text: &str) -> Progress {
        ron::from_str(text).unwrap_or_default()
    }
}

/// Days since 1970-01-01 (UTC) now.
pub fn today() -> i64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    seconds.div_euclid(86_400)
}

/// "2026-10-08" for a day counted from 1970-01-01.
pub fn date_label(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-date, proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// "1 h 05 min", "12 min", "40 s".
pub fn duration_label(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{} h {:02} min", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(score: f64, streak: u32, timing_ms: Option<f64>) -> Outcome {
        Outcome {
            score,
            streak,
            timing_ms,
        }
    }

    #[test]
    fn dates() {
        assert_eq!(date_label(0), "1970-01-01");
        assert_eq!(date_label(-1), "1969-12-31");
        assert_eq!(date_label(11_016), "2000-02-29");
        assert_eq!(date_label(20_734), "2026-10-08");
        assert!(today() > 20_000);
    }

    #[test]
    fn keeps_the_best_run() {
        let mut p = Progress::default();
        assert!(p.finish(
            Kind::Exercise,
            "C major",
            "",
            outcome(0.8, 5, Some(40.0)),
            "2026-10-01"
        ));
        assert!(!p.finish(
            Kind::Exercise,
            "C major",
            "",
            outcome(0.7, 9, None),
            "2026-10-02"
        ));
        let record = &p.exercises["C major"];
        assert_eq!(record.runs, 2);
        assert_eq!(record.last.as_deref(), Some("2026-10-02"));
        let best = &record.bests[""];
        assert_eq!(best.score, 0.8);
        assert_eq!(best.streak, 9, "the longest streak is kept on its own");
        assert_eq!(best.date, "2026-10-01");
        // Same score, tighter timing: better.
        assert!(p.finish(
            Kind::Exercise,
            "C major",
            "",
            outcome(0.8, 1, Some(20.0)),
            "2026-10-03"
        ));
        assert_eq!(p.exercises["C major"].bests[""].timing_ms, Some(20.0));
        // Songs keep one best per variant.
        p.finish(
            Kind::Song,
            "Ode",
            "Wait · Right",
            outcome(1.0, 0, None),
            "d",
        );
        p.finish(
            Kind::Song,
            "Ode",
            "Play along · Both",
            outcome(0.5, 0, Some(60.0)),
            "d",
        );
        assert_eq!(p.songs["Ode"].bests.len(), 2);
    }

    #[test]
    fn practice_time_adds_up_by_day() {
        let mut p = Progress::default();
        p.practise(Kind::Exercise, "Reading", 30.0, "2026-10-07");
        p.practise(Kind::Song, "Ode", 90.0, "2026-10-08");
        p.practise(Kind::Song, "Ode", 30.0, "2026-10-08");
        p.practise(Kind::Song, "Ode", -5.0, "2026-10-08");
        assert_eq!(p.songs["Ode"].seconds, 120.0);
        assert_eq!(p.total_seconds(), 150.0);
        let days = p.recent_days(20_734, 3);
        assert_eq!(
            days,
            [
                ("2026-10-06".to_owned(), 0.0),
                ("2026-10-07".to_owned(), 30.0),
                ("2026-10-08".to_owned(), 120.0)
            ]
        );
    }

    #[test]
    fn saves_and_loads_as_ron() {
        let mut p = Progress::default();
        p.finish(
            Kind::Exercise,
            "Chords",
            "",
            outcome(0.9, 3, None),
            "2026-10-08",
        );
        p.practise(Kind::Exercise, "Chords", 12.5, "2026-10-08");
        let text = p.to_ron();
        assert_eq!(Progress::from_ron(&text), p);
        assert_eq!(Progress::from_ron("garbage"), Progress::default());
        // Fields added later default when an old file lacks them.
        assert_eq!(Progress::from_ron("(exercises: {})"), Progress::default());
    }

    #[test]
    fn durations() {
        assert_eq!(duration_label(40.2), "40 s");
        assert_eq!(duration_label(720.0), "12 min");
        assert_eq!(duration_label(3900.0), "1 h 05 min");
    }
}
