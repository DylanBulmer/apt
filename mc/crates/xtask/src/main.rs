//! Build-time tooling. Never packaged, never installed.
//!
//! `scripts/build.sh` runs this on the host it is building on, which is why it
//! is a workspace member rather than a hidden flag on `mc` itself: the
//! generator has no business shipping inside `/usr/bin/mc`, and CI builds every
//! architecture on a native runner, so a host-built helper is always runnable.
//!
//!   xtask man <dir>   write mc.1 into <dir>

mod man;

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("man"), Some(dir)) => write_man(PathBuf::from(dir)),
        _ => Err("usage: xtask man <dir>".to_string()),
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

fn write_man(dir: PathBuf) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("mc.1");
    let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    man::render(&mut file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}
