//! `mc serve` — systemd's `ExecStart=`.
//!
//! RUNS AS THE `minecraft` USER under `ProtectSystem=strict`, not as root. It
//! must therefore take no privilege guard and write nothing outside `MC_BASE`.
//! The shell version enforced this by physically splitting the library in two
//! (`common.sh` for the unprivileged side, `lib.sh` for root); here it is a
//! declared [`Requirement::ServiceAccount`] plus the test that asserts it.
//!
//! The gates below live here rather than in `mc start` because
//! `systemctl start minecraft` bypasses the CLI entirely — this is the only
//! thing standing between a stray start and a server running on a licence
//! nobody accepted, or on compiled-in defaults nobody chose.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use mc_common::error::{Error, Result};
use mc_common::{Config, config, eula, java};

use crate::context::Ctx;

pub fn run(ctx: &Ctx) -> Result<()> {
    let cfg = Config::load(&ctx.paths)?;
    let base = ctx.paths.base();

    // ── EULA gate ──────────────────────────────────────────────────────────
    //
    // Checked before the JVM is launched for two reasons. The server's own
    // check writes a default eula.txt and exits almost immediately, which
    // systemd reports as a start followed by a puzzling stop with nothing
    // useful in the journal. And this is the path `systemctl start` takes.
    if !eula::accepted(&ctx.paths.eula()) {
        // Not "re-run mc install": that re-downloads the server jar.
        // `mc start` accepts the flag precisely so an existing server has a
        // cheap way back.
        return Err(Error::config(format!(
            "The Minecraft EULA has not been accepted.\n\
             {}\n\
             Accept it with: mc start --accept-eula\n\
             or set eula=true in {}",
            eula::EULA_URL,
            ctx.paths.eula().display()
        )));
    }

    // ── server.properties access gate ──────────────────────────────────────
    //
    // The JVM treats an UNREADABLE server.properties as an absent one: it logs
    // a stack trace, reports "Failed to store properties to file", and carries
    // on with compiled-in defaults — stock port, RCON off, level-name "world".
    // The server appears to start normally while ignoring every setting the
    // operator configured, and if level-name was customised it generates a new
    // empty world beside the real one. Fail here, where the reason is legible.
    //
    // The usual cause is a root-owned file: 0640 is readable only because the
    // owner is the service account, and editing it as root with an editor that
    // writes and renames replaces it with a root-owned inode.
    //
    // An absent file is fine — that is a first boot, and the server writes its
    // own.
    let props = ctx.paths.server_properties();
    if props.exists() && !is_readable_and_writable(&props) {
        let user = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
            .ok()
            .flatten()
            .map(|u| u.name)
            .unwrap_or_else(|| "this user".to_string());
        return Err(Error::config(format!(
            "{} is not readable and writable by {user}.\n\
             The server would silently start on default settings.\n\
             Fix with: chown {}:{} {} && chmod 640 {}",
            props.display(),
            mc_common::paths::MC_USER,
            mc_common::paths::MC_USER,
            props.display(),
            props.display()
        )));
    }

    // ── Resolve the runtime ────────────────────────────────────────────────
    let configured = cfg.java_major();
    let java_bin = java::find_binary(ctx.paths.root(), configured).unwrap_or_else(|| {
        mc_common::ui::warn(format!(
            "Java {configured} not found; falling back to system java"
        ));
        PathBuf::from("java")
    });
    // Flags are chosen from the runtime that will ACTUALLY run, not from the
    // one the config asked for: the fallback above may have landed elsewhere,
    // and passing a Java 21 flag set to a Java 17 JVM is a boot failure.
    let actual = java::major_version(&java_bin).unwrap_or(17);
    let flags = cfg.java_flags(actual);

    let run_sh = ctx.paths.run_sh();
    if run_sh.is_file() {
        return exec_run_sh(&base, &run_sh, &java_bin, &cfg, &flags);
    }

    let jar = ctx.paths.server_jar();
    if !jar.is_file() {
        return Err(Error::config(format!(
            "server.jar not found in {}\n       Install one with: mc install",
            base.display()
        )));
    }

    let mut command = Command::new(&java_bin);
    command
        .current_dir(&base)
        .arg(format!("-Xmx{}", cfg.java.ram))
        .arg(format!("-Xms{}", cfg.java.ram))
        .args(&flags)
        .args(&cfg.java.opts)
        .args(["-jar", "server.jar", "nogui"]);

    // exec, not spawn: the unit is Type=simple and systemd is watching THIS
    // pid. A child would leave an idle shim as the main process, so a JVM crash
    // would look like a clean exit and Restart=on-failure would never fire.
    Err(Error::other(format!(
        "could not exec the server: {}",
        command.exec()
    )))
}

/// NeoForge's launcher takes its JVM arguments from a file rather than argv.
fn exec_run_sh(
    base: &Path,
    run_sh: &Path,
    java_bin: &Path,
    cfg: &config::Config,
    flags: &[String],
) -> Result<()> {
    let mut args = vec![
        format!("-Xmx{}", cfg.java.ram),
        format!("-Xms{}", cfg.java.ram),
    ];
    args.extend(flags.iter().cloned());
    args.extend(cfg.java.opts.iter().cloned());

    let user_args = base.join("user_jvm_args.txt");
    std::fs::write(&user_args, format!("{}\n", args.join("\n")))
        .map_err(|e| Error::io(&user_args, e))?;

    // run.sh resolves the JVM through JAVA_HOME or PATH, so the version chosen
    // above has to be handed over explicitly or the launcher picks its own.
    let java_home = java_bin.parent().and_then(Path::parent);

    let mut command = Command::new("bash");
    command.current_dir(base).arg(run_sh).arg("nogui");
    if let Some(home) = java_home {
        command.env("JAVA_HOME", home);
    }
    Err(Error::other(format!(
        "could not exec the server: {}",
        command.exec()
    )))
}

fn is_readable_and_writable(path: &Path) -> bool {
    use nix::unistd::{AccessFlags, access};
    access(path, AccessFlags::R_OK | AccessFlags::W_OK).is_ok()
}
