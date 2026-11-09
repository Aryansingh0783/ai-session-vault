//! Start `avault daemon` automatically at login: a scheduled task on Windows, a launchd agent on
//! macOS, a systemd user unit on Linux. Nothing is installed unless `enable` is run without `--print`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const TASK_NAME: &str = "AI Session Vault";
const LABEL: &str = "dev.ai-session-vault.daemon";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Enable,
    Disable,
    Status,
}

/// The exact command the OS will run at login.
pub fn daemon_args(vault: &Path, log: &Path) -> Vec<String> {
    vec!["--vault".into(), vault.display().to_string(), "daemon".into(), "--log".into(), log.display().to_string()]
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Windows scheduled-task action: a hidden PowerShell that runs the daemon.
pub fn windows_task_command(exe: &Path, args: &[String]) -> String {
    let inner = args.iter().map(|a| format!("'{}'", a.replace('\'', "''"))).collect::<Vec<_>>().join(" ");
    format!("powershell.exe -NoProfile -WindowStyle Hidden -Command \"& '{}' {}\"", exe.display().to_string().replace('\'', "''"), inner)
}

pub fn launchd_plist(exe: &Path, args: &[String]) -> String {
    let mut items = format!("    <string>{}</string>\n", xml(&exe.display().to_string()));
    for a in args {
        items.push_str(&format!("    <string>{}</string>\n", xml(a)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n  <key>Label</key><string>{LABEL}</string>\n  <key>ProgramArguments</key><array>\n{items}  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n</dict></plist>\n"
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn systemd_unit(exe: &Path, args: &[String]) -> String {
    let cmd = std::iter::once(quote(&exe.display().to_string())).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
    format!("[Unit]\nDescription=AI Session Vault engine\n\n[Service]\nExecStart={cmd}\nRestart=on-failure\nRestartSec=30\n\n[Install]\nWantedBy=default.target\n")
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("could not run {:?}", cmd.get_program()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("{}", text.trim());
    }
    Ok(text)
}

pub fn apply(action: Action, vault: &Path, log: &Path, print_only: bool) -> Result<String> {
    let exe = std::env::current_exe()?;
    let args = daemon_args(vault, log);
    if cfg!(windows) {
        let tr = windows_task_command(&exe, &args);
        let create = ["/Create", "/F", "/SC", "ONLOGON", "/RL", "LIMITED", "/TN", TASK_NAME, "/TR", &tr];
        return match action {
            Action::Enable if print_only => Ok(format!("schtasks {}", create.iter().map(|a| if a.contains(' ') { quote(a) } else { a.to_string() }).collect::<Vec<_>>().join(" "))),
            Action::Enable => {
                run(Command::new("schtasks").args(create))?;
                Ok(format!("Scheduled task '{TASK_NAME}' created: the engine starts at login. Start it now with: schtasks /Run /TN \"{TASK_NAME}\""))
            }
            Action::Disable => {
                run(Command::new("schtasks").args(["/Delete", "/F", "/TN", TASK_NAME]))?;
                Ok("Autostart removed.".into())
            }
            Action::Status => match run(Command::new("schtasks").args(["/Query", "/TN", TASK_NAME])) {
                Ok(t) => Ok(format!("Autostart is enabled.\n{}", t.trim())),
                Err(_) => Ok("Autostart is not enabled.".into()),
            },
        };
    }
    let home = dirs::home_dir().context("no home folder")?;
    if cfg!(target_os = "macos") {
        let plist = home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"));
        return match action {
            Action::Enable if print_only => Ok(format!("{}:\n{}", plist.display(), launchd_plist(&exe, &args))),
            Action::Enable => {
                std::fs::create_dir_all(plist.parent().unwrap())?;
                std::fs::write(&plist, launchd_plist(&exe, &args))?;
                run(Command::new("launchctl").arg("load").arg(&plist))?;
                Ok(format!("Launch agent installed: {}", plist.display()))
            }
            Action::Disable => {
                let _ = run(Command::new("launchctl").arg("unload").arg(&plist));
                let _ = std::fs::remove_file(&plist);
                Ok("Autostart removed.".into())
            }
            Action::Status => Ok(if plist.exists() { "Autostart is enabled.".into() } else { "Autostart is not enabled.".to_string() }),
        };
    }
    let unit: PathBuf = home.join(".config/systemd/user/ai-session-vault.service");
    match action {
        Action::Enable if print_only => Ok(format!("{}:\n{}", unit.display(), systemd_unit(&exe, &args))),
        Action::Enable => {
            std::fs::create_dir_all(unit.parent().unwrap())?;
            std::fs::write(&unit, systemd_unit(&exe, &args))?;
            run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run(Command::new("systemctl").args(["--user", "enable", "--now", "ai-session-vault.service"]))?;
            Ok(format!("systemd user service installed and started: {}", unit.display()))
        }
        Action::Disable => {
            let _ = run(Command::new("systemctl").args(["--user", "disable", "--now", "ai-session-vault.service"]));
            let _ = std::fs::remove_file(&unit);
            Ok("Autostart removed.".into())
        }
        Action::Status => Ok(if unit.exists() { "Autostart is enabled.".into() } else { "Autostart is not enabled.".to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_command_quotes_paths_with_spaces_and_apostrophes() {
        let c = windows_task_command(Path::new("C:\\Program Files\\avault.exe"), &daemon_args(Path::new("D:\\Aryan's Vault"), Path::new("C:\\l.log")));
        assert!(c.starts_with("powershell.exe -NoProfile -WindowStyle Hidden"));
        assert!(c.contains("'C:\\Program Files\\avault.exe'") && c.contains("'D:\\Aryan''s Vault'"), "{c}");
    }

    #[test]
    fn unit_and_plist_contain_the_daemon_command() {
        let args = daemon_args(Path::new("/v"), Path::new("/l"));
        assert!(systemd_unit(Path::new("/bin/avault"), &args).contains("ExecStart=\"/bin/avault\" \"--vault\" \"/v\" \"daemon\""));
        let p = launchd_plist(Path::new("/bin/a&b"), &args);
        assert!(p.contains("/bin/a&amp;b") && p.contains("<key>RunAtLoad</key>"));
    }
}
