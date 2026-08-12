//! Static completion scripts — core commands only.
//!
//! Generated at build time and shipped in the mc-server package so completions
//! work immediately after install. `mc completions <shell>` is the authoritative
//! source and supersedes this when plugins are installed.

use std::path::Path;

use clap::CommandFactory as _;
use clap_complete::{Shell, generate};

pub fn generate_shell(shell: Shell, out_dir: &Path) -> std::io::Result<()> {
    let mut command = mc::cli::Cli::command();
    let filename = match shell {
        Shell::Bash => "mc",
        Shell::Zsh => "_mc",
        // parse_shell in main.rs only returns Bash or Zsh, so this is
        // unreachable, but the match must be exhaustive.
        s => return Err(std::io::Error::other(format!("unsupported shell: {s:?}"))),
    };
    let path = out_dir.join(filename);

    // Generate to a buffer first so we can post-process.
    let mut buf = Vec::new();
    generate(shell, &mut command, "mc", &mut buf);
    let script = String::from_utf8(buf).map_err(std::io::Error::other)?;

    // Post-process for bash: only show subcommands unless a dash is typed,
    // and hide short flags to reduce noise.
    let output = match shell {
        Shell::Bash => postprocess_bash(&script),
        _ => script,
    };

    std::fs::write(&path, output)?;
    Ok(())
}

/// Post-process a bash completion script to improve usability:
/// - Only show subcommands when no dash is typed
/// - Only show long flags when a dash is typed (short flags hidden)
fn postprocess_bash(script: &str) -> String {
    // First pass: split opts= lines into flags= and cmds= variables.
    let mut result = String::with_capacity(script.len());

    for line in script.lines() {
        if let Some(opts_match) = extract_opts(line) {
            let (flags, cmds) = split_opts(&opts_match);
            let indent = line.find('o').unwrap_or(0);
            let prefix = &line[..indent];
            result.push_str(prefix);
            result.push_str("opts=\"");
            result.push_str(&opts_match);
            result.push('"');
            result.push('\n');
            result.push_str(prefix);
            result.push_str("flags=\"");
            result.push_str(&flags);
            result.push('"');
            result.push('\n');
            result.push_str(prefix);
            result.push_str("cmds=\"");
            result.push_str(&cmds);
            result.push('"');
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Second pass: replace the completion logic line-by-line.
    let lines: Vec<&str> = result.lines().collect();
    let mut output = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines.get(i).copied().unwrap_or("");
        if line.contains("${cur} == -* || ${COMP_CWORD} -eq ") {
            let indent = line.find("if").unwrap_or(0);
            let prefix = &line[..indent];
            let cword = extract_comp_cword(line);
            output.push_str(prefix);
            output.push_str("if [[ ${cur} == --* ]]; then\n");
            output.push_str(prefix);
            output.push_str("    COMPREPLY=( $(compgen -W \"${flags}\" -- \"${cur}\") )\n");
            output.push_str(prefix);
            output.push_str("    return 0\n");
            output.push_str(prefix);
            output.push_str("elif [[ ${COMP_CWORD} -eq ");
            output.push_str(&cword.to_string());
            output.push_str(" ]]; then\n");
            output.push_str(prefix);
            output.push_str("    COMPREPLY=( $(compgen -W \"${cmds}\" -- \"${cur}\") )\n");
            output.push_str(prefix);
            output.push_str("    return 0\n");
            i += 3;
            if lines.get(i).is_some_and(|l| l.trim() == "fi") {
                output.push_str(prefix);
                output.push_str("fi\n");
                i += 1;
            }
        } else {
            output.push_str(line);
            output.push('\n');
            i += 1;
        }
    }

    output
}

/// Extract the value from an opts= line.
fn extract_opts(line: &str) -> Option<String> {
    let opts_start = line.find("opts=\"")?;
    let value_start = opts_start + 6;
    let value_end = line.rfind('"')?;
    if value_end > value_start {
        Some(line[value_start..value_end].to_string())
    } else {
        None
    }
}

/// Split opts into flags (starting with -) and commands (not starting with -).
fn split_opts(opts: &str) -> (String, String) {
    let mut flags = Vec::new();
    let mut cmds = Vec::new();

    for word in opts.split_whitespace() {
        if word.starts_with('-') {
            flags.push(word);
        } else {
            cmds.push(word);
        }
    }

    (flags.join(" "), cmds.join(" "))
}

/// Extract the COMP_CWORD value from a line.
fn extract_comp_cword(line: &str) -> u8 {
    for (cword, pattern) in [
        (1, "-eq 1"),
        (2, "-eq 2"),
        (3, "-eq 3"),
        (4, "-eq 4"),
        (5, "-eq 5"),
    ] {
        if line.contains(pattern) {
            return cword;
        }
    }
    1
}
