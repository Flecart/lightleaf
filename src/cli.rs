//! Command-line account management, run on the server itself:
//!
//! ```text
//! lightleaf user add <email> [--name NAME] [--admin] [--password-stdin]
//! lightleaf user passwd <email> [--password-stdin]
//! lightleaf user admin <email> on|off
//! lightleaf user logout <email>
//! lightleaf user list
//! ```
//!
//! It uses the same environment (`LIGHTLEAF_DATA`, ...) as the server and works while the server
//! is running. In Docker: `docker compose exec lightleaf lightleaf user add --admin you@example.com`.

use crate::{auth, state::AppState};
use anyhow::{Context, bail};
use rusqlite::{OptionalExtension, params};
use std::io::Read;

pub const USAGE: &str = "\
Lightleaf — self-hosted LaTeX & Typst editor

USAGE:
    lightleaf                       start the server (configured with LIGHTLEAF_* variables)
    lightleaf user add <email> [--name NAME] [--admin] [--password-stdin]
                                    create an account (a random password is printed
                                    unless --password-stdin is given)
    lightleaf user passwd <email> [--password-stdin]
                                    reset a password and log the user out everywhere
    lightleaf user admin <email> on|off
                                    grant or remove administrator rights
    lightleaf user logout <email>   log the user out of every browser
    lightleaf user list             list accounts
    lightleaf help                  show this message
";

struct Flags {
    positional: Vec<String>,
    name: Option<String>,
    admin: bool,
    password_stdin: bool,
}

fn parse(args: &[String]) -> anyhow::Result<Flags> {
    let mut f = Flags {
        positional: vec![],
        name: None,
        admin: false,
        password_stdin: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--admin" => f.admin = true,
            "--password-stdin" => f.password_stdin = true,
            "--name" => f.name = Some(it.next().context("--name needs a value")?.clone()),
            s if s.starts_with("--name=") => f.name = Some(s["--name=".len()..].to_string()),
            s if s.starts_with("--") => bail!("unknown option {s}\n\n{USAGE}"),
            s => f.positional.push(s.to_string()),
        }
    }
    Ok(f)
}

/// Returns the password from stdin, or a freshly generated one (and whether it was generated).
fn obtain_password(from_stdin: bool) -> anyhow::Result<(String, bool)> {
    if from_stdin {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        let pw = s.trim_end_matches(['\r', '\n']).to_string();
        auth::validate_password(&pw).map_err(|e| anyhow::anyhow!(e.1))?;
        Ok((pw, false))
    } else {
        Ok((auth::random_password(), true))
    }
}

async fn user_id(state: &AppState, email: &str) -> anyhow::Result<i64> {
    let e = email.trim().to_string();
    state
        .db(move |c| {
            c.query_row("SELECT id FROM users WHERE email = ?1", [&e], |r| r.get(0))
                .optional()
        })
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?
        .with_context(|| format!("no account with email {email}"))
}

pub async fn run(state: &AppState, args: &[String]) -> anyhow::Result<()> {
    let (cmd, rest) = match args {
        [group, cmd, rest @ ..] if group == "user" => (cmd.as_str(), rest),
        _ => bail!("{USAGE}"),
    };
    let f = parse(rest)?;
    let email = f.positional.first().map(String::as_str);
    match (cmd, email) {
        ("add", Some(email)) => {
            let (pw, generated) = obtain_password(f.password_stdin)?;
            let name = f
                .name
                .clone()
                .unwrap_or_else(|| email.split('@').next().unwrap_or(email).to_string());
            auth::insert_user(state, email, &name, &pw, Some(f.admin))
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            println!(
                "Created {}{email}.",
                if f.admin { "administrator " } else { "" }
            );
            if generated {
                println!("Password: {pw}\n(The user can change it in Account settings.)");
            }
        }
        ("passwd", Some(email)) => {
            let uid = user_id(state, email).await?;
            let (pw, generated) = obtain_password(f.password_stdin)?;
            auth::set_password(state, uid, pw.clone(), None)
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            println!("Password of {email} reset; all of their sessions were logged out.");
            if generated {
                println!("New password: {pw}");
            }
        }
        ("admin", Some(email)) => {
            let on = match f.positional.get(1).map(String::as_str) {
                Some("on") | Some("true") | Some("yes") => true,
                Some("off") | Some("false") | Some("no") => false,
                _ => bail!("usage: lightleaf user admin <email> on|off"),
            };
            let uid = user_id(state, email).await?;
            state
                .db(move |c| {
                    c.execute(
                        "UPDATE users SET is_admin = ?2 WHERE id = ?1",
                        params![uid, on as i64],
                    )
                })
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            println!(
                "{email} is {} an administrator.",
                if on { "now" } else { "no longer" }
            );
        }
        ("logout", Some(email)) => {
            let uid = user_id(state, email).await?;
            let n = state
                .db(move |c| c.execute("DELETE FROM sessions WHERE user_id = ?1", [uid]))
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            println!("Logged {email} out of {n} session(s).");
        }
        ("list", _) => {
            let rows: Vec<(String, String, bool)> = state
                .db(|c| {
                    let mut st =
                        c.prepare("SELECT email, name, is_admin FROM users ORDER BY id")?;
                    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))?
                        .collect()
                })
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            if rows.is_empty() {
                println!("No accounts yet. Create one with: lightleaf user add --admin <email>");
            }
            for (email, name, admin) in rows {
                println!("{email:<40} {name}{}", if admin { "  [admin]" } else { "" });
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
