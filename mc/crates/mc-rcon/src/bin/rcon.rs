//! `/usr/bin/rcon` — the standalone client.
//!
//! Kept as its own binary rather than folded into the plugin: it is useful by
//! hand, it is what a member of the `minecraft` group can already run, and the
//! plugin's own interface (`mc-rcon command rcon …`) is not one anybody should
//! script against.

use std::io::Read as _;
use std::path::PathBuf;

use mc_rcon::protocol::Connection;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rcon: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: rcon --password-file FILE HOST PORT [command...]\n\
     \n\
     With no command, opens an interactive session.\n\
     The password is read from a file so it never appears in argv,\n\
     which is world-readable through /proc/<pid>/cmdline."
        .to_string()
}

fn run(args: &[String]) -> Result<(), String> {
    let mut password_file: Option<PathBuf> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--password-file" => {
                password_file = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or_else(|| "--password-file needs a path".to_string())?,
                ));
                i += 2;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            // Everything after the host and port is the command, and it may
            // legitimately start with '-'.
            _ if positional.len() >= 2 => {
                positional.extend(args.get(i..).into_iter().flatten().map(String::as_str));
                break;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }

    let host = positional.first().ok_or_else(usage)?;
    let port: u16 = positional
        .get(1)
        .ok_or_else(usage)?
        .parse()
        .map_err(|_| format!("invalid port: {}", positional.get(1).unwrap_or(&"")))?;

    let file = password_file.ok_or_else(|| {
        // Never an argv option: /proc/<pid>/cmdline is world-readable, so a
        // password passed as an argument is visible to every local user for the
        // lifetime of the process.
        "--password-file is required; the password is never taken from argv".to_string()
    })?;
    let password = std::fs::read_to_string(&file)
        .map_err(|e| format!("{}: {e}", file.display()))?
        .trim()
        .to_string();

    let mut connection = Connection::connect(host, port).map_err(|e| e.to_string())?;
    connection
        .authenticate(&password)
        .map_err(|e| e.to_string())?;

    let command: Vec<&str> = positional.get(2..).unwrap_or_default().to_vec();
    if !command.is_empty() {
        let reply = connection
            .exec(&command.join(" "))
            .map_err(|e| e.to_string())?;
        println!("{}", reply.trim_end());
        return Ok(());
    }

    interactive(&mut connection)
}

fn interactive(connection: &mut Connection) -> Result<(), String> {
    use std::io::Write as _;
    let stdin = std::io::stdin();
    let interactive = std::io::IsTerminal::is_terminal(&stdin);

    loop {
        if interactive {
            print!("rcon> ");
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        let mut handle = stdin.lock();
        let mut byte = [0u8; 1];
        // Read a line without pulling in a dependency, and without buffering
        // past the newline: the socket and stdin are both live here.
        loop {
            match handle.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => line.push(char::from(byte[0])),
                Err(e) => return Err(e.to_string()),
            }
        }
        if line.is_empty() && byte[0] != b'\n' {
            if interactive {
                println!();
            }
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            return Ok(());
        }
        match connection.exec(line) {
            Ok(reply) => println!("{}", reply.trim_end()),
            Err(e) => eprintln!("rcon: {e}"),
        }
    }
}
