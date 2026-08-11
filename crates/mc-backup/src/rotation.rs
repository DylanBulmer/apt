//! Retention.

use std::path::{Path, PathBuf};

/// Archives this plugin creates, newest first by name.
///
/// The name carries a sortable timestamp (`minecraft-YYYYmmdd-HHMMSS.tar.gz`),
/// so lexicographic order IS chronological order — and unlike mtime it cannot
/// be perturbed by a copy or a restore-from-tape.
pub fn sorted_archives(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut archives: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_archive(p))
        .collect();
    archives.sort();
    archives.reverse();
    archives
}

fn is_archive(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("minecraft-") && name.ends_with(".tar.gz"))
}

/// Which archives to delete to honour `keep`.
///
/// `keep = 0` disables rotation entirely rather than deleting everything — the
/// dangerous reading of the same value, and the one an operator setting it to
/// zero would least expect.
pub fn prune_list(archives: &[PathBuf], keep: u32) -> Vec<PathBuf> {
    if keep == 0 {
        return Vec::new();
    }
    archives.iter().skip(keep as usize).cloned().collect()
}

/// A timestamped archive name.
pub fn archive_name(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("minecraft-{}.tar.gz", format_utc(secs))
}

/// `YYYYmmdd-HHMMSS` in UTC, without pulling in a date library.
fn format_utc(epoch_secs: u64) -> String {
    let days = epoch_secs / 86_400;
    let time = epoch_secs % 86_400;
    let (hour, minute, second) = (time / 3600, (time % 3600) / 60, time % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// Howard Hinnant's days-from-civil, inverted. Exact for the whole proleptic
/// Gregorian calendar, which is more than this needs but avoids an off-by-one
/// around leap years that a hand-rolled approximation would introduce.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archives(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn keeps_the_newest_and_prunes_the_rest() {
        let list = archives(&[
            "minecraft-20260810-120000.tar.gz",
            "minecraft-20260809-120000.tar.gz",
            "minecraft-20260808-120000.tar.gz",
            "minecraft-20260807-120000.tar.gz",
        ]);
        let pruned = prune_list(&list, 2);
        assert_eq!(
            pruned,
            archives(&[
                "minecraft-20260808-120000.tar.gz",
                "minecraft-20260807-120000.tar.gz"
            ])
        );
    }

    #[test]
    fn keep_zero_disables_rotation_rather_than_deleting_everything() {
        // The dangerous reading of the same value, and the one an operator
        // setting it to zero would least expect.
        let list = archives(&["minecraft-20260810-120000.tar.gz"]);
        assert!(prune_list(&list, 0).is_empty());
    }

    #[test]
    fn keeping_more_than_exist_prunes_nothing() {
        let list = archives(&["minecraft-20260810-120000.tar.gz"]);
        assert!(prune_list(&list, 7).is_empty());
    }

    #[test]
    fn only_this_plugins_archives_are_candidates() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "minecraft-20260810-120000.tar.gz",
            "minecraft-20260809-120000.tar.gz",
            // An operator's own copy, and unrelated files: never pruned.
            "before-the-big-upgrade.tar.gz",
            "notes.txt",
            "minecraft-20260810-120000.tar.gz.sha256",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let found = sorted_archives(dir.path());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].to_string_lossy().contains("20260810"),
            "newest first"
        );
    }

    #[test]
    fn names_sort_chronologically() {
        // Lexicographic order is chronological order for this format, and
        // unlike mtime it survives a copy.
        let mut list = archives(&[
            "minecraft-20260809-235959.tar.gz",
            "minecraft-20260810-000000.tar.gz",
            "minecraft-20251231-235959.tar.gz",
        ]);
        list.sort();
        assert_eq!(
            list.last().map(|p| p.to_string_lossy().into_owned()),
            Some("minecraft-20260810-000000.tar.gz".to_string())
        );
    }

    #[test]
    fn formats_a_timestamp_the_calendar_agrees_with() {
        // 2026-08-10T12:34:56Z. Written as an expression rather than a magic
        // number so the date being asserted is visible.
        const DAYS_TO_2026_08_10: u64 = 20_675;
        let epoch = DAYS_TO_2026_08_10 * 86_400 + 12 * 3600 + 34 * 60 + 56;
        assert_eq!(format_utc(epoch), "20260810-123456");
    }

    #[test]
    fn formats_the_epoch_itself() {
        assert_eq!(format_utc(0), "19700101-000000");
    }

    #[test]
    fn handles_a_leap_day_correctly() {
        // 2024-02-29T00:00:00Z — a hand-rolled approximation gets this wrong.
        const DAYS_TO_2024_02_29: u64 = 19_782;
        assert_eq!(format_utc(DAYS_TO_2024_02_29 * 86_400), "20240229-000000");
    }
}
