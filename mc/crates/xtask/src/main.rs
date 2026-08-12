//! Build-time tooling. Never packaged, never installed.
//!
//! `scripts/build.sh` runs this on the host it is building on, which is why it
//! is a workspace member rather than a hidden flag on `mc` itself: the
//! generator has no business shipping inside `/usr/bin/mc`, and CI builds every
//! architecture on a native runner, so a host-built helper is always runnable.
//!
//!   xtask man <dir>                    write mc.1 into <dir>
//!   xtask completions <bash|zsh> <dir> write completion scripts into <dir>

mod completions;
mod man;

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) {
        (Some("man"), Some(dir), None) => write_man(PathBuf::from(dir)),
        (Some("completions"), Some(shell), Some(dir)) => {
            write_completions(shell, PathBuf::from(dir))
        }
        _ => Err("usage: xtask man <dir> | xtask completions <bash|zsh> <dir>".to_string()),
    };

    match result {
        Ok(path) => {
            println!("wrote {}", path.display());
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("xtask: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn parse_shell(s: &str) -> Result<clap_complete::Shell, String> {
    match s {
        "bash" => Ok(clap_complete::Shell::Bash),
        "zsh" => Ok(clap_complete::Shell::Zsh),
        _ => Err(format!("unsupported shell: {s} (supported: bash, zsh)")),
    }
}

fn write_completions(shell: &str, dir: PathBuf) -> Result<PathBuf, String> {
    let shell = parse_shell(shell)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    completions::generate_shell(shell, &dir).map_err(|e| format!("completions: {e}"))?;
    Ok(dir)
}

fn write_man(dir: PathBuf) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("mc.1");
    let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    man::render(&mut file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}
