//! Which Java runtime a given Minecraft version needs, and where to find it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Map a Minecraft version to the Java major version it requires.
///
/// The input is UNTRUSTED: `cmd_install_mrpack`'s equivalent takes the
/// `minecraft` field straight out of a `.mrpack` manifest and this ran as root.
/// In bash that mattered enormously — `[[ "$major" -ge 26 ]]` is an arithmetic
/// context, and bash performs command substitution inside array subscripts
/// while evaluating one, so `PATH[$(rm -rf /)]` executed even under
/// `set -euo pipefail`. Rust has no such context; parsing to an integer and
/// treating anything else as 0 preserves the same fail-safe answer.
pub fn required_major(mc_version: &str) -> u32 {
    let mut parts = mc_version.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);

    // Mojang switched to a new versioning scheme after 1.21.x: versions 26.x.x
    // and above use the new format and require Java 25.
    if major >= 26 {
        25
    } else if minor >= 21 || (minor == 20 && patch >= 5) {
        21
    } else if minor >= 18 {
        17
    } else {
        8
    }
}

/// Parse the major version out of a `java -version` banner.
///
/// Java 8 reports `1.8.0_452`; everything since reports `21.0.4`. Returns
/// `None` when the runtime formats its banner in a way we do not recognise —
/// callers pick a default rather than proceeding on a guess.
pub fn parse_version_banner(banner: &str) -> Option<u32> {
    let quoted = banner
        .lines()
        .find(|l| l.contains("version"))?
        .split('"')
        .nth(1)?;
    let rest = quoted.strip_prefix("1.").unwrap_or(quoted);
    rest.split(['.', '_', '-']).next()?.parse().ok()
}

/// The major version of a java binary, or `None` if it cannot be determined.
pub fn major_version(bin: &Path) -> Option<u32> {
    let out = Command::new(bin).arg("-version").output().ok()?;
    // The banner goes to stderr, not stdout, on every runtime that matters.
    let banner = String::from_utf8_lossy(&out.stderr);
    parse_version_banner(&banner)
}

/// Directories a distribution might install a JRE into, formatted with `{}` for
/// the major version. Ordered by how likely they are on Debian.
const CANDIDATE_TEMPLATES: [&str; 7] = [
    "/usr/lib/jvm/java-{}-openjdk-amd64/bin/java",
    "/usr/lib/jvm/java-{}-openjdk-arm64/bin/java",
    "/usr/lib/jvm/java-{}-openjdk/bin/java",
    "/usr/lib/jvm/temurin-{}-amd64/bin/java",
    "/usr/lib/jvm/temurin-{}/bin/java",
    "/usr/lib/jvm/java-{}-amazon-corretto-amd64/bin/java",
    "/usr/lib/jvm/java-{}-amazon-corretto/bin/java",
];

/// Locate a java binary for the given major version.
///
/// `update-alternatives` first, because that is what the operator or the
/// distribution actually selected; the fixed candidate list is the fallback for
/// a runtime installed outside the alternatives system.
pub fn find_binary(root: &Path, required: u32) -> Option<PathBuf> {
    if let Ok(out) = Command::new("update-alternatives")
        .args(["--list", "java"])
        .output()
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let path = Path::new(line);
            // Match the version as a whole token: `-21/` must not be satisfied
            // by `-217`, and `java-8-` must not match `java-8u...` builds we
            // did not mean.
            if path.is_file() && path_names_version(line, required) {
                return Some(path.to_path_buf());
            }
        }
    }

    for template in CANDIDATE_TEMPLATES {
        let candidate = root.join(
            template
                .replace("{}", &required.to_string())
                .trim_start_matches('/'),
        );
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// True when `path` contains `-<major>` followed by a non-digit or the end.
fn path_names_version(path: &str, required: u32) -> bool {
    let needle = format!("-{required}");
    let mut from = 0;
    while let Some(idx) = path.get(from..).and_then(|s| s.find(&needle)) {
        let at = from + idx;
        let after = at + needle.len();
        match path.get(after..).and_then(|s| s.chars().next()) {
            None => return true,
            Some(c) if !c.is_ascii_digit() => return true,
            _ => {}
        }
        from = after;
    }
    false
}

// ── GC flag presets ────────────────────────────────────────────────────────
//
// Java 8  → Aikar's G1GC. UnlockExperimentalVMOptions is required because
//           several of these tuning flags were still experimental there.
// Java 17 → the same flags WITHOUT the unlock, which became unnecessary in
//           Java 9–11 and could silently enable other experimental behaviour.
// Java 21+→ Generational ZGC: sub-millisecond pauses.

const AIKAR_COMMON: &[&str] = &[
    "-XX:+UseG1GC",
    "-XX:+ParallelRefProcEnabled",
    "-XX:MaxGCPauseMillis=200",
    "-XX:+DisableExplicitGC",
    "-XX:+AlwaysPreTouch",
    "-XX:G1NewSizePercent=30",
    "-XX:G1MaxNewSizePercent=40",
    "-XX:G1HeapRegionSize=8M",
    "-XX:G1ReservePercent=20",
    "-XX:G1HeapWastePercent=5",
    "-XX:G1MixedGCCountTarget=4",
    "-XX:InitiatingHeapOccupancyPercent=15",
    "-XX:G1MixedGCLiveThresholdPercent=90",
    "-XX:G1RSetUpdatingPauseTimePercent=5",
    "-XX:SurvivorRatio=32",
    "-XX:+PerfDisableSharedMem",
    "-XX:MaxTenuringThreshold=1",
];

const ZGC: &[&str] = &[
    "-XX:+UseZGC",
    "-XX:-ZUncommit",
    "-XX:+AlwaysPreTouch",
    "-XX:+DisableExplicitGC",
];

/// The GC flags to launch with, when the operator has not chosen their own.
///
/// `-XX:+ZGenerational` is applied ONLY to Java 21–23, never folded into the
/// ZGC preset. It arrived experimental in 21, became the ZGC default in 23, and
/// was REMOVED in 24. On 24+ it produces a warning and identical behaviour —
/// harmless today, but obsolete VM options do not stay ignored: the JVM's path
/// is ignored → deprecated → rejected, and an option that reaches
/// "Unrecognized VM option" makes the JVM refuse to start at all. That would
/// take the server down on a routine `apt upgrade` of the JRE, with a failure
/// that looks nothing like its cause.
pub fn default_flags(java_major: u32) -> Vec<String> {
    let mut flags: Vec<String> = if java_major >= 21 {
        ZGC.iter().map(|s| s.to_string()).collect()
    } else if java_major >= 17 {
        AIKAR_COMMON.iter().map(|s| s.to_string()).collect()
    } else {
        let mut v: Vec<String> = AIKAR_COMMON.iter().map(|s| s.to_string()).collect();
        v.push("-XX:+UnlockExperimentalVMOptions".to_string());
        v
    };

    if (21..=23).contains(&java_major) {
        flags.push("-XX:+ZGenerational".to_string());
    }
    flags
}

/// The apt package providing a headless JRE of the given major version.
pub fn jre_package(major: u32) -> String {
    format!("openjdk-{major}-jre-headless")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_minecraft_versions_to_runtimes() {
        assert_eq!(required_major("1.16.5"), 8);
        assert_eq!(required_major("1.18.2"), 17);
        assert_eq!(required_major("1.20.4"), 17);
        // 1.20.5 is the boundary Mojang moved the requirement at.
        assert_eq!(required_major("1.20.5"), 21);
        assert_eq!(required_major("1.21"), 21);
        assert_eq!(required_major("1.21.9"), 21);
        // The post-1.21 scheme.
        assert_eq!(required_major("26.2"), 25);
        assert_eq!(required_major("27.0.1"), 25);
    }

    /// KNOWN GAP, carried over deliberately rather than fixed here.
    ///
    /// The thresholds are `minor >= 18 -> 17`, so 1.17.x resolves to Java 8 —
    /// but 1.17 actually needs Java 16, which Debian 13 does not ship either.
    /// The same arithmetic is wrong for an older pinned NeoForge (`21.1.66`
    /// parses as minor=1 and lands on 8).
    ///
    /// This test exists to make the gap visible and to fail loudly if someone
    /// "fixes" the thresholds without also deciding what to do about NeoForge's
    /// version scheme, which now overlaps Minecraft's own.
    #[test]
    fn known_gap_versions_that_resolve_to_the_wrong_runtime() {
        assert_eq!(required_major("1.17"), 8, "1.17 really needs Java 16");
        assert_eq!(
            required_major("21.1.66"),
            8,
            "an old pinned NeoForge version"
        );
    }

    #[test]
    fn hostile_version_components_never_reach_an_evaluator() {
        // In bash each of these was a code-execution sink. Here the requirement
        // is only that they parse to the fail-safe answer and touch nothing.
        for hostile in [
            "PATH[$(touch /tmp/mc-java-canary)]",
            "$(id)",
            "`whoami`",
            "",
            "....",
            "-1.-1.-1",
        ] {
            assert_eq!(required_major(hostile), 8, "{hostile:?}");
        }
        assert!(!Path::new("/tmp/mc-java-canary").exists());
    }

    #[test]
    fn parses_both_banner_generations() {
        assert_eq!(
            parse_version_banner("openjdk version \"1.8.0_452\"\nOpenJDK Runtime Environment"),
            Some(8)
        );
        assert_eq!(
            parse_version_banner("openjdk version \"21.0.4\" 2024-07-16"),
            Some(21)
        );
        assert_eq!(
            parse_version_banner("openjdk version \"25\" 2026-09-16"),
            Some(25)
        );
        assert_eq!(parse_version_banner("garbage with no quotes"), None);
        assert_eq!(parse_version_banner(""), None);
    }

    #[test]
    fn version_token_match_is_not_a_substring_match() {
        assert!(path_names_version(
            "/usr/lib/jvm/java-21-openjdk-amd64/bin/java",
            21
        ));
        assert!(path_names_version("/usr/lib/jvm/temurin-8/bin/java", 8));
        // The bug this guards: `-21` must not be satisfied by `-217`.
        assert!(!path_names_version(
            "/usr/lib/jvm/java-217-openjdk/bin/java",
            21
        ));
        assert!(!path_names_version(
            "/usr/lib/jvm/java-17-openjdk/bin/java",
            1
        ));
    }

    #[test]
    fn zgenerational_applies_only_where_the_flag_exists() {
        // Java 24 removed it; an unrecognised VM option stops the JVM booting.
        for major in [8, 17, 24, 25] {
            assert!(
                !default_flags(major)
                    .iter()
                    .any(|f| f.contains("ZGenerational")),
                "java {major} must not be passed ZGenerational"
            );
        }
        for major in [21, 22, 23] {
            assert!(
                default_flags(major)
                    .iter()
                    .any(|f| f == "-XX:+ZGenerational")
            );
        }
    }

    #[test]
    fn gc_preset_matches_the_runtime_generation() {
        assert!(default_flags(25).contains(&"-XX:+UseZGC".to_string()));
        assert!(default_flags(21).contains(&"-XX:+UseZGC".to_string()));
        assert!(default_flags(17).contains(&"-XX:+UseG1GC".to_string()));
        assert!(default_flags(8).contains(&"-XX:+UseG1GC".to_string()));

        // The unlock flag belongs to Java 8 alone.
        let unlock = "-XX:+UnlockExperimentalVMOptions".to_string();
        assert!(default_flags(8).contains(&unlock));
        assert!(!default_flags(17).contains(&unlock));
    }
}
