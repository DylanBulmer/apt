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
    let mut file = std::fs::File::create(&path)?;
    generate(shell, &mut command, "mc", &mut file);
    Ok(())
}
