//! `mc.1`, assembled from the real clap tree and two prose fragments.
//!
//! The COMMANDS section is walked out of the same [`clap::Command`] the binary
//! parses with, so a subcommand or a flag cannot ship undocumented — the same
//! reason completions are generated rather than maintained. Everything a parser
//! cannot know (what the exit codes mean to systemd, which files are read,
//! which pages to look at next) is prose in `crates/mc/man/`, appended around
//! it.

use std::io::Write;

use clap::CommandFactory as _;
use roff::{Roff, bold, italic, roman};

/// NAME, SYNOPSIS and DESCRIPTION — everything above the command list.
const HEAD: &str = include_str!("../../mc/man/mc.1.head.roff");
/// EXIT STATUS onwards — everything the clap tree cannot answer.
const TAIL: &str = include_str!("../../mc/man/mc.1.tail.roff");

/// The version stamped into the page footer.
///
/// xtask shares the workspace version, which is kept in step with the
/// `Version:` in `DEBIAN/control` — see the note in the workspace manifest.
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn render(out: &mut dyn Write) -> std::io::Result<()> {
    let mut command = mc::cli::Cli::command();
    // Populates the argument metadata the renderer below reads: value ranges,
    // possible values, and the auto-generated --help.
    command.build();

    // Written literally rather than through Roff::control, which cannot emit a
    // quoted empty argument: an unquoted empty date collapses under groff's
    // whitespace rules and every later field shifts up one.
    //
    // The date is empty on purpose. A build date would make two builds of the
    // same source differ, and the page is already versioned by the package it
    // ships in.
    writeln!(out, r#".TH MC 1 "" "mc {VERSION}" "mc manual""#)?;

    out.write_all(HEAD.as_bytes())?;
    commands(&command).to_writer(out)?;
    out.write_all(TAIL.as_bytes())?;
    Ok(())
}

/// One `.TP` entry per subcommand, each with its own options nested under it.
///
/// Nested rather than a single flat OPTIONS section because mc has no global
/// options: every flag belongs to one command, and `--force` means a different
/// thing to `install` than to `upgrade`.
fn commands(command: &clap::Command) -> Roff {
    let mut roff = Roff::default();
    roff.control("SH", ["COMMANDS"]);

    for sub in visible(command) {
        roff.control("TP", []);
        roff.text([bold(format!("mc {}", sub.get_name())), roman(usage(sub))]);
        if let Some(about) = sub.get_about() {
            roff.text([roman(format!("{about}."))]);
        }

        let args: Vec<&clap::Arg> = documented(sub).collect();
        if !args.is_empty() {
            // .RS/.RE indents the option list under its command rather than
            // letting it read as a sibling of the next command.
            roff.control("RS", []);
            for arg in args {
                roff.control("TP", []);
                roff.text(spec(arg));
                if let Some(help) = arg.get_help() {
                    roff.text([roman(format!("{help}."))]);
                }
                let values: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .map(|v| v.get_name().to_string())
                    .collect();
                if !values.is_empty() {
                    roff.text([roman(format!("One of: {}.", values.join(", ")))]);
                }
            }
            roff.control("RE", []);
        }
    }
    roff
}

/// Subcommands an operator types.
///
/// `serve`, `shutdown` and `reload` are hidden here because they are systemd's
/// `ExecStart=`/`ExecStop=`/`ExecReload=` rather than an interface — the tail
/// fragment documents them as the unit's contract, which is what someone
/// reading about them actually needs. `help` is clap's own.
fn visible(command: &clap::Command) -> impl Iterator<Item = &clap::Command> {
    command
        .get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != "help")
}

/// Arguments worth a line of their own: the ones clap generates for `--help`
/// and `--version` are not.
///
/// Matched by action rather than by name. `mc install --version` is a real
/// option that takes a Minecraft version, and filtering on the id would drop
/// it from the page while leaving the page looking complete.
fn documented(sub: &clap::Command) -> impl Iterator<Item = &clap::Arg> {
    sub.get_arguments().filter(|a| {
        !a.is_hide_set()
            && !matches!(
                a.get_action(),
                clap::ArgAction::Help
                    | clap::ArgAction::HelpShort
                    | clap::ArgAction::HelpLong
                    | clap::ArgAction::Version
            )
    })
}

/// The bracketed argument summary that follows the command name.
fn usage(sub: &clap::Command) -> String {
    let mut parts = Vec::new();
    for arg in documented(sub).filter(|a| !a.is_positional()) {
        parts.push(format!("[{}]", flag(arg)));
    }
    for arg in documented(sub).filter(|a| a.is_positional()) {
        let name = value_name(arg);
        parts.push(if arg.is_required_set() {
            name
        } else {
            format!("[{name}]")
        });
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" "))
    }
}

/// The tag line of one option: flags bold, placeholders italic.
fn spec(arg: &clap::Arg) -> Vec<roff::Inline> {
    if arg.is_positional() {
        return vec![italic(value_name(arg))];
    }
    let mut inlines = Vec::new();
    if let Some(short) = arg.get_short() {
        inlines.push(bold(format!("-{short}")));
        inlines.push(roman(", "));
    }
    if let Some(long) = arg.get_long() {
        inlines.push(bold(format!("--{long}")));
    }
    if takes_value(arg) {
        inlines.push(roman(" "));
        inlines.push(italic(value_name(arg)));
    }
    inlines
}

/// The same thing as one unstyled string, for the usage summary.
fn flag(arg: &clap::Arg) -> String {
    let mut spec = match (arg.get_short(), arg.get_long()) {
        (_, Some(long)) => format!("--{long}"),
        (Some(short), None) => format!("-{short}"),
        (None, None) => arg.get_id().to_string(),
    };
    if takes_value(arg) {
        spec.push(' ');
        spec.push_str(&value_name(arg));
    }
    spec
}

fn takes_value(arg: &clap::Arg) -> bool {
    arg.get_num_args().is_some_and(|range| range.takes_values())
}

fn value_name(arg: &clap::Arg) -> String {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map(|name| name.to_string())
        .unwrap_or_else(|| arg.get_id().to_string().to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> String {
        let mut buf = Vec::new();
        render(&mut buf).expect("rendering to a Vec cannot fail");
        String::from_utf8(buf).expect("roff output is utf8")
    }

    #[test]
    fn the_manual_documents_every_subcommand() {
        // The property that makes generating this worth the machinery: a
        // subcommand added to cli.rs and nowhere else still reaches the page.
        let mut command = mc::cli::Cli::command();
        command.build();
        let page = page();

        for sub in visible(&command) {
            assert!(
                page.contains(&format!("mc {}", sub.get_name())),
                "`mc {}` is missing from mc.1",
                sub.get_name()
            );
        }
    }

    #[test]
    fn the_manual_documents_every_flag() {
        let mut command = mc::cli::Cli::command();
        command.build();
        let page = page();

        for sub in visible(&command) {
            for arg in documented(sub).filter(|a| !a.is_positional()) {
                if let Some(long) = arg.get_long() {
                    // Every hyphen is backslash-escaped in the generated
                    // sections, including the ones inside the flag's own name.
                    let escaped = long.replace('-', r"\-");
                    assert!(
                        page.contains(&format!(r"\-\-{escaped}")),
                        "--{long} of `mc {}` is missing from mc.1",
                        sub.get_name()
                    );
                }
            }
        }
    }

    #[test]
    fn the_systemd_exec_targets_are_documented_as_prose() {
        // They are hidden from the command list on purpose — they are not an
        // interface — but an operator reading the unit file needs to find
        // them, and the exit-78 contract with them.
        let page = page();
        for target in ["serve", "shutdown", "reload"] {
            assert!(page.contains(target), "`mc {target}` is missing from mc.1");
        }
        assert!(page.contains("78"), "the EX_CONFIG contract is missing");
    }

    #[test]
    fn the_manual_points_at_the_pages_the_plugins_ship() {
        // Plugin subcommands are not in the clap tree, so mc.1 cannot document
        // them. It must at least say where they are.
        // The prose fragments are raw roff, so a cross-reference is written
        // the way man(7) wants it and not the way the escaper would.
        let page = page();
        for reference in [".BR mc-rcon (1)", ".BR mc-backup (1)", ".BR mc-config (5)"] {
            assert!(page.contains(reference), "SEE ALSO is missing {reference}");
        }
    }

    #[test]
    fn the_page_declares_its_title_and_section() {
        // Quoted empty date: unquoted, groff shifts the footer fields up one
        // and the version becomes the date.
        assert!(page().contains(r#".TH MC 1 "" "mc "#), "malformed .TH line");
    }
}
