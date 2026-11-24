//! Episode-detection heuristics. Every heuristic can be switched off in the
//! settings, so a disc that confuses one of them can still be handled.
//!
//! Pipeline: classify_titles -> assign_episode_numbers -> check_name_conflict
//! (-> review_reason in Automatic Mode). The web UI and Automatic Mode use the
//! same pipeline; only what happens with an ambiguous result differs (a
//! banner for a human vs. parking the disc in the review queue).

use crate::config::Config;
use crate::robot::{find_episode_hint, Hint, Title};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

pub const SHORT: &str = "short";
pub const LONG: &str = "long";
pub const OUTLIER: &str = "outlier";
pub const PLAY_ALL: &str = "play_all";
pub const HINT: &str = "hint";

/// `null` in JSON becomes the default value instead of an error.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One disc title as shown in the editable table.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Row {
    pub title_index: i64,
    #[serde(deserialize_with = "nullable")]
    pub duration_text: String,
    pub duration_sec: i64,
    pub chapters: i64,
    pub size_bytes: i64,
    #[serde(deserialize_with = "nullable")]
    pub source_file: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    pub include: bool,
    pub special: bool,
    pub episode: Option<i64>,
    #[serde(deserialize_with = "nullable")]
    pub episode_title: String,
    #[serde(deserialize_with = "nullable")]
    pub warnings: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub flags: Vec<String>,
    pub hint: Option<Hint>,
}

impl Row {
    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
}

fn fmt_titles(rows: &[Row], idx: &[usize]) -> String {
    let t: Vec<i64> = idx.iter().map(|&i| rows[i].title_index).collect();
    if t.len() > 2 {
        format!("{}-{}", t[0], t[t.len() - 1])
    } else {
        t.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(" + ")
    }
}

/// A "Play All" title plays several episodes back to back, so its length
/// equals a run of consecutive other titles added up. Each title's length is
/// rounded to the second, so one second of slack per summed title is
/// allowed. Checked longest-first; a title found to be a Play All is not
/// used as a part of another one.
fn find_play_all(rows: &mut [Row]) {
    let included: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].include).collect();
    let mut order = included.clone();
    order.sort_by_key(|&i| -rows[i].duration_sec);
    let mut marked: HashSet<usize> = HashSet::new();
    for &r in &order {
        let dur = rows[r].duration_sec;
        if dur <= 0 {
            continue;
        }
        let others: Vec<usize> = included
            .iter()
            .copied()
            .filter(|&o| o != r && !marked.contains(&o) && rows[o].duration_sec > 0)
            .collect();
        let mut found: Option<Vec<usize>> = None;
        'outer: for i in 0..others.len() {
            let mut total = 0;
            for j in i..others.len() {
                total += rows[others[j]].duration_sec;
                let count = (j - i + 1) as i64;
                let slack = count + 1;
                if count >= 2 && (total - dur).abs() <= slack {
                    found = Some(others[i..=j].to_vec());
                    break 'outer;
                }
                if total > dur + slack {
                    break;
                }
            }
        }
        if let Some(parts) = found {
            let desc = fmt_titles(rows, &parts);
            marked.insert(r);
            let row = &mut rows[r];
            row.include = false;
            row.flags.push(PLAY_ALL.into());
            row.warnings.push(format!(
                "Looks like a \"Play All\" title: as long as titles {desc} combined. \
                 Unticked; tick it again if it really is one long episode."
            ));
        }
    }
}

/// Length filter, Play All detection, long-title warning, disc-name hints
/// and duration-cluster outliers. Returns the rows plus notes worth logging.
pub fn classify_titles(titles: &[Title], cfg: &Config) -> (Vec<Row>, Vec<String>) {
    let mut rows: Vec<Row> = titles
        .iter()
        .map(|t| Row {
            title_index: t.title_index,
            duration_text: t.duration_text.clone(),
            duration_sec: t.duration_sec,
            chapters: t.chapters,
            size_bytes: t.size_bytes,
            source_file: t.source_file.clone(),
            name: t.name.clone(),
            include: true,
            ..Default::default()
        })
        .collect();

    if cfg.detect_length_filter {
        for row in rows.iter_mut().filter(|r| r.duration_sec < cfg.min_length_sec) {
            row.include = false;
            row.special = true;
            row.flags.push(SHORT.into());
            row.warnings.push(
                "Short - looks like a menu/extra, not an episode. Marked Special; \
                 uncheck Special if this IS a real (short) episode."
                    .into(),
            );
        }
    }

    if cfg.detect_play_all {
        find_play_all(&mut rows);
    }

    if cfg.detect_long_warn {
        for row in rows.iter_mut().filter(|r| r.include && r.duration_sec > cfg.long_warn_sec) {
            row.flags.push(LONG.into());
            row.warnings.push(format!(
                "Over {} min - this may be several episodes combined into one title \
                 (\"Play All\"). Check the disc menu; you may need to split it.",
                cfg.long_warn_sec / 60
            ));
        }
    }

    if cfg.detect_names {
        let hints: Vec<Option<Hint>> = titles.iter().map(|t| find_episode_hint(&t.text_values())).collect();
        // The same name on several titles is a disc label, not an episode name.
        for (row, hint) in rows.iter_mut().zip(hints.iter()) {
            let Some(h) = hint else { continue };
            let same_source = hints.iter().flatten().filter(|o| o.source == h.source).count();
            if same_source == 1 {
                row.flags.push(HINT.into());
                row.warnings.push(format!(
                    "Disc metadata suggests: Episode {} (from '{}', {} confidence)",
                    h.episode, h.source, h.confidence
                ));
                row.hint = Some(h.clone());
            }
        }
    }

    let mut notes = Vec::new();
    if cfg.detect_cluster {
        let candidates: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].include).collect();
        if candidates.len() >= 2 {
            // Bucket by ~2 minutes; the biggest bucket is the typical episode.
            let mut buckets: Vec<(i64, Vec<usize>)> = Vec::new();
            for &i in &candidates {
                let key = (rows[i].duration_sec as f64 / 120.0).round_ties_even() as i64;
                match buckets.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, v)) => v.push(i),
                    None => buckets.push((key, vec![i])),
                }
            }
            let best_len = buckets.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
            let top = &buckets.iter().find(|(_, v)| v.len() == best_len).unwrap().1;
            if top.len() >= 2 {
                let typical = top.iter().map(|&i| rows[i].duration_sec as f64).sum::<f64>() / top.len() as f64;
                if typical > 0.0 {
                    for &i in &candidates {
                        let r = &mut rows[i];
                        if (r.duration_sec as f64 - typical).abs() / typical > cfg.cluster_tolerance {
                            r.flags.push(OUTLIER.into());
                            r.warnings.push(format!(
                                "Duration is unusually different from this disc's typical episode \
                                 length (~{}m) - double-check this is really an episode.",
                                (typical / 60.0).round_ties_even() as i64
                            ));
                        }
                    }
                }
            } else {
                notes.push(
                    "Couldn't detect a typical episode length automatically (durations too varied) - \
                     relying on the length threshold only."
                        .into(),
                );
            }
        }
    }
    (rows, notes)
}

/// Keeps Episode # sequential and gap-free among included, non-Special rows
/// (in disc order), starting at `next_episode`. Included Specials without a
/// number are numbered from `next_special`. Excluded rows get no number.
pub fn assign_episode_numbers(rows: &mut [Row], next_episode: i64, next_special: i64) {
    let mut ep = next_episode;
    let mut sp = next_special.max(1);
    for row in rows.iter_mut() {
        if row.include && !row.special {
            row.episode = Some(ep);
            ep += 1;
        } else if row.include && row.special {
            if row.episode.is_none() {
                row.episode = Some(sp);
            }
            sp = sp.max(row.episode.unwrap_or(0) + 1);
        } else {
            row.episode = None;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConflictExample {
    pub title_index: i64,
    pub source: String,
    pub episode: i64,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conflict {
    pub conflict: bool,
    pub partial: bool,
    pub hinted_indices: Vec<usize>,
    pub candidate_count: usize,
    pub hinted_count: usize,
    pub examples: Vec<ConflictExample>,
}

/// Does the disc-name detector disagree with (or only partly cover) the
/// default disc-order numbering? None if there's nothing to ask about.
pub fn check_name_conflict(rows: &[Row]) -> Option<Conflict> {
    let candidates: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].include).collect();
    let hinted: Vec<usize> = candidates.iter().copied().filter(|&i| rows[i].hint.is_some()).collect();
    if hinted.is_empty() {
        return None;
    }
    let conflict = hinted.iter().any(|&i| rows[i].episode != rows[i].hint.as_ref().map(|h| h.episode));
    let partial = hinted.len() < candidates.len();
    if !(conflict || partial) {
        return None;
    }
    Some(Conflict {
        conflict,
        partial,
        candidate_count: candidates.len(),
        hinted_count: hinted.len(),
        examples: hinted
            .iter()
            .take(5)
            .map(|&i| {
                let h = rows[i].hint.as_ref().unwrap();
                ConflictExample {
                    title_index: rows[i].title_index,
                    source: h.source.clone(),
                    episode: h.episode,
                    confidence: h.confidence.clone(),
                }
            })
            .collect(),
        hinted_indices: hinted,
    })
}

/// Why an unattended rip should stop and ask, or None if it's safe to go
/// ahead. A wrong guess costs a mis-numbered season, so anything the
/// heuristics flagged on a title that would be ripped counts.
pub fn review_reason(rows: &[Row], conflict: Option<&Conflict>) -> Option<String> {
    let mut reasons = Vec::new();
    if let Some(c) = conflict {
        if c.conflict {
            reasons.push("detected episode names disagree with disc-order numbering".to_string());
        }
        if c.partial {
            reasons.push(format!("only {} of {} titles had a detected name", c.hinted_count, c.candidate_count));
        }
    }
    let list = |flag: &str| -> Vec<String> {
        rows.iter().filter(|r| r.include && r.has_flag(flag)).map(|r| r.title_index.to_string()).collect()
    };
    let long = list(LONG);
    if !long.is_empty() {
        reasons.push(format!("title(s) {} are over the long-title limit", long.join(", ")));
    }
    let odd = list(OUTLIER);
    if !odd.is_empty() {
        reasons.push(format!("title(s) {} don't match the typical episode length", odd.join(", ")));
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

pub fn apply_name_hints(rows: &mut [Row], hinted: &[usize]) {
    for &i in hinted {
        if let Some(h) = rows.get(i).and_then(|r| r.hint.clone()) {
            rows[i].episode = Some(h.episode);
        }
    }
}

/// Identifies a physical disc by its label plus the length and size of every
/// title, so the same disc reinserted (or still in the drive after a restart)
/// isn't ripped twice, while different discs sharing a generic label
/// ("DISC1") still differ. Same value as the Python version produced.
pub fn disc_fingerprint(disc_name: &str, titles: &[Title]) -> String {
    let mut parts = vec![disc_name.to_string()];
    parts.extend(titles.iter().map(|t| format!("{}:{}:{}", t.title_index, t.duration_sec, t.size_bytes)));
    sha1_smol::Sha1::from(parts.join("|")).digest().to_string()
}

/// Ticked, ready-to-rip rows only, checked for duplicate episodes per group
/// (regular vs. Special: S01E01 and S00E01 can coexist, two S01E01 can't).
pub fn validate_rip_rows(rows: &[Row]) -> Result<Vec<Row>, String> {
    let mut seen = HashSet::new();
    let mut picked = Vec::new();
    for r in rows.iter().filter(|r| r.include) {
        let ep = match r.episode {
            Some(e) if e > 0 => e,
            other => {
                return Err(format!(
                    "Title {} has an invalid episode number: {}",
                    r.title_index,
                    other.map(|e| e.to_string()).unwrap_or_else(|| "empty".into())
                ))
            }
        };
        if !seen.insert((r.special, ep)) {
            let what = if r.special { format!("Special episode {ep}") } else { format!("Episode {ep}") };
            return Err(format!("{what} is used more than once. Fix duplicates first."));
        }
        picked.push(r.clone());
    }
    if picked.is_empty() {
        return Err("No titles selected to rip.".into());
    }
    picked.sort_by_key(|r| (r.special, r.episode));
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::robot::{parse_disc_info, tests::REAL_SCAN};

    fn title(idx: i64, sec: i64, name: &str) -> Title {
        Title { title_index: idx, duration_sec: sec, name: name.into(), ..Default::default() }
    }

    fn classify(titles: &[Title], cfg: &Config, next: i64) -> Vec<Row> {
        let (mut rows, _) = classify_titles(titles, cfg);
        assign_episode_numbers(&mut rows, next, 1);
        rows
    }

    #[test]
    fn real_disc() {
        let rows = classify(&parse_disc_info(REAL_SCAN).titles, &Config::default(), 14);
        assert!(!rows[0].include && rows[0].has_flag(PLAY_ALL));
        assert_eq!(rows[1..4].iter().map(|r| r.episode).collect::<Vec<_>>(), vec![Some(14), Some(15), Some(16)]);
        assert!(rows[4].special && !rows[4].include);
        assert!(rows[5].special && !rows[5].include);
        assert_eq!(review_reason(&rows, check_name_conflict(&rows).as_ref()), None);
    }

    #[test]
    fn without_play_all_detection_auto_mode_asks() {
        let cfg = Config { detect_play_all: false, ..Default::default() };
        let rows = classify(&parse_disc_info(REAL_SCAN).titles, &cfg, 14);
        assert!(rows[0].include);
        assert!(review_reason(&rows, None).unwrap().contains("long-title"));
    }

    #[test]
    fn long_double_episode_is_not_play_all() {
        let rows = classify(&[title(0, 1327, ""), title(1, 1326, ""), title(2, 2680, "")], &Config::default(), 14);
        assert!(rows.iter().all(|r| r.include));
    }

    #[test]
    fn play_all_of_blu_ray_style_disc() {
        let t = [title(0, 2600, ""), title(1, 2650, ""), title(2, 2610, ""), title(3, 7861, "")];
        let rows = classify(&t, &Config::default(), 14);
        assert!(!rows[3].include);
        assert_eq!(rows[..3].iter().map(|r| r.episode).collect::<Vec<_>>(), vec![Some(14), Some(15), Some(16)]);
    }

    #[test]
    fn shared_disc_label_is_not_a_hint() {
        let rows = classify(&[title(0, 1300, "SHOW Episode 1"), title(1, 1300, "SHOW Episode 1")], &Config::default(), 1);
        assert!(rows.iter().all(|r| r.hint.is_none()));
    }

    #[test]
    fn name_hint_conflict() {
        let rows = classify(&[title(0, 1300, "Episode 3"), title(1, 1300, "Episode 1")], &Config::default(), 1);
        let c = check_name_conflict(&rows).unwrap();
        assert!(c.conflict);
        assert!(review_reason(&rows, Some(&c)).unwrap().contains("disagree"));
        let mut rows = rows;
        apply_name_hints(&mut rows, &c.hinted_indices);
        assert_eq!(rows.iter().map(|r| r.episode).collect::<Vec<_>>(), vec![Some(3), Some(1)]);
    }

    #[test]
    fn specials_are_numbered_from_next_special() {
        let mut rows = classify(&[title(0, 1300, ""), title(1, 300, ""), title(2, 200, "")], &Config::default(), 5);
        rows[1].include = true;
        rows[2].include = true;
        assign_episode_numbers(&mut rows, 5, 3);
        assert_eq!(rows.iter().map(|r| r.episode).collect::<Vec<_>>(), vec![Some(5), Some(3), Some(4)]);
    }

    #[test]
    fn fingerprint_matches_python_and_changes_with_layout() {
        let disc = parse_disc_info(REAL_SCAN);
        let a = disc_fingerprint(&disc.disc_name, &disc.titles);
        let b = disc_fingerprint(&disc.disc_name, &disc.titles[..5]);
        assert_ne!(a, b);
        // As computed by the old Python app, so its "already ripped" list stays valid.
        assert_eq!(a, "2f508fd6c26eb25935e8a6a78504cfc47ddd61f8");
    }

    #[test]
    fn validation() {
        let mk = |ep: Option<i64>, special: bool| Row { include: true, episode: ep, special, ..Default::default() };
        assert!(validate_rip_rows(&[mk(Some(1), false), mk(Some(1), true)]).is_ok());
        assert!(validate_rip_rows(&[mk(Some(1), false), mk(Some(1), false)]).unwrap_err().contains("more than once"));
        assert!(validate_rip_rows(&[mk(None, false)]).is_err());
        assert!(validate_rip_rows(&[]).is_err());
    }

    #[test]
    fn rows_accept_nulls_from_old_database() {
        let r: Row = serde_json::from_str(r#"{"title_index":2,"episode":null,"episode_title":null,"include":true}"#).unwrap();
        assert_eq!(r.episode_title, "");
        assert_eq!(r.episode, None);
    }
}
