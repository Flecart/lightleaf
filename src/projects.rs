use crate::{
    auth::User,
    collab::flush_project,
    compile::is_known_compiler,
    error::{AppError, AppResult},
    git::{commit_all, init_repo},
    overleaf::{choose_compiler, detect, import_zip as import_zip_file},
    state::{Role, SharedState},
    util::{clean_rel_path, now},
};
use axum::{
    Json,
    extract::{Multipart, Path, State},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

const LATEX_TEMPLATE: &str = r#"\documentclass{article}
\usepackage[utf8]{inputenc}
\usepackage{amsmath}
\usepackage{graphicx}
\usepackage{hyperref}

\title{Untitled Document}
\author{Your Name}
\date{\today}

\begin{document}
\maketitle

\begin{abstract}
Write your abstract here.
\end{abstract}

\section{Introduction}
Welcome to Lightleaf! Edit this file and press \textbf{Recompile} (Ctrl+S) to update the preview.

\begin{equation}
  e^{i\pi} + 1 = 0
\end{equation}

\end{document}
"#;

const TYPST_TEMPLATE: &str = r#"#set document(title: "Untitled Document")
#set page(paper: "a4", numbering: "1")
#set text(size: 11pt)
#set heading(numbering: "1.")

#align(center, text(17pt)[*Untitled Document*])
#align(center)[Your Name]

= Introduction
Welcome to Lightleaf! Edit this file and press *Recompile* (Ctrl+S) to update the preview.

$ e^(i pi) + 1 = 0 $
"#;

pub const GITIGNORE: &str =
    "*.aux\n*.log\n*.out\n*.synctex.gz\n*.fdb_latexmk\n*.fls\n*.bbl\n*.blg\n*.toc\n";

fn project_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "name": r.get::<_, String>(1)?,
        "compiler": r.get::<_, String>(2)?,
        "mainFile": r.get::<_, String>(3)?,
        "createdAt": r.get::<_, i64>(4)?,
        "updatedAt": r.get::<_, i64>(5)?,
        "owner": { "id": r.get::<_, i64>(6)?, "name": r.get::<_, String>(7)?, "email": r.get::<_, String>(8)? },
        "role": r.get::<_, String>(9)?,
    }))
}

const PROJECT_SELECT: &str =
    "SELECT p.id, p.name, p.compiler, p.main_file, p.created_at, p.updated_at,
        u.id, u.name, u.email, CASE WHEN p.owner_id = ?1 THEN 'owner' ELSE m.role END
     FROM projects p JOIN users u ON u.id = p.owner_id
     LEFT JOIN members m ON m.project_id = p.id AND m.user_id = ?1";

pub async fn list(State(state): State<SharedState>, user: User) -> AppResult<Json<Value>> {
    let rows = state
        .db(move |c| {
            let mut st = c.prepare(&format!(
                "{PROJECT_SELECT} WHERE p.owner_id = ?1 OR m.user_id IS NOT NULL ORDER BY p.updated_at DESC"
            ))?;
            st.query_map([user.id], project_json)?.collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!(rows)))
}

async fn insert_project(
    state: &SharedState,
    user: &User,
    name: &str,
    compiler: &str,
    main: &str,
) -> AppResult<String> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let (pid, name, compiler, main, uid) = (
        id.clone(),
        name.to_string(),
        compiler.to_string(),
        main.to_string(),
        user.id,
    );
    state
        .db(move |c| {
            c.execute(
                "INSERT INTO projects (id, name, owner_id, compiler, main_file, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![pid, name, uid, compiler, main, now()],
            )
        })
        .await?;
    Ok(id)
}

fn clean_name(name: &str) -> AppResult<String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 150 {
        return Err(AppError::bad("project name must be 1-150 characters"));
    }
    Ok(n.to_string())
}

#[derive(Deserialize)]
pub struct CreateReq {
    name: String,
    #[serde(default)]
    template: Option<String>,
}

pub async fn create(
    State(state): State<SharedState>,
    user: User,
    Json(req): Json<CreateReq>,
) -> AppResult<Json<Value>> {
    let name = clean_name(&req.name)?;
    let (compiler, main, content) = match req.template.as_deref() {
        Some("typst") => ("typst", "main.typ", TYPST_TEMPLATE),
        Some("blank") => (
            "pdflatex",
            "main.tex",
            "\\documentclass{article}\n\\begin{document}\n\n\\end{document}\n",
        ),
        _ => ("pdflatex", "main.tex", LATEX_TEMPLATE),
    };
    let id = insert_project(&state, &user, &name, compiler, main).await?;
    let dir = state.cfg.project_dir(&id);
    init_repo(&dir).await?;
    tokio::fs::write(dir.join(main), content).await?;
    if compiler != "typst" {
        tokio::fs::write(dir.join(".gitignore"), GITIGNORE).await?;
    }
    commit_all(&dir, None, "Create project", Some(&user)).await?;
    Ok(Json(json!({ "id": id })))
}

/// Creates a project from an uploaded .zip archive (Overleaf "Download → Source" zips,
/// arXiv source bundles, or any zip of a LaTeX/Typst project).
pub async fn import_zip(
    State(state): State<SharedState>,
    user: User,
    mut mp: Multipart,
) -> AppResult<Json<Value>> {
    let mut name = None;
    let mut data = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| AppError::bad(e.to_string()))?
    {
        match field.name() {
            Some("name") => name = Some(field.text().await.unwrap_or_default()),
            Some("file") => {
                if name.is_none() {
                    // Overleaf exports are named `Project_Name.zip`.
                    name = field
                        .file_name()
                        .map(|f| f.trim_end_matches(".zip").replace('_', " "));
                }
                data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| AppError::bad(e.to_string()))?,
                );
            }
            _ => {}
        }
    }
    let data = data.ok_or_else(|| AppError::bad("missing file"))?;
    let name = clean_name(
        &name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Imported project".into()),
    )?;
    let tmp = state
        .cfg
        .data_dir
        .join(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let t = tmp.clone();
    let res = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&t)?;
        let report = import_zip_file(&data, &t)?;
        Ok::<_, anyhow::Error>((report, detect(&t)))
    })
    .await
    .map_err(anyhow::Error::from)?;
    let (report, det) = match res {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(AppError::bad(format!("could not import zip: {e}")));
        }
    };
    let compiler = choose_compiler(&det.wanted, &crate::compile::available_compilers());
    let id = insert_project(&state, &user, &name, &compiler, &det.main_file).await?;
    let dir = state.cfg.project_dir(&id);
    tokio::fs::rename(&tmp, &dir).await?;
    init_repo(&dir).await?;
    commit_all(&dir, None, "Import project", Some(&user)).await?;
    let note = (compiler != det.wanted)
        .then(|| format!("{} is not installed here, using {compiler}", det.wanted));
    Ok(Json(json!({
        "id": id,
        "name": name,
        "mainFile": det.main_file,
        "compiler": compiler,
        "reason": det.reason,
        "note": note,
        "files": report.files,
        "skipped": report.skipped.len(),
    })))
}

pub async fn get(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let pid = id.clone();
    let (mut project, members) = state
        .db(move |c| {
            let p = c.query_row(&format!("{PROJECT_SELECT} WHERE p.id = ?2"), params![user.id, pid], project_json)?;
            let mut st = c.prepare(
                "SELECT u.id, u.name, u.email, m.role FROM members m JOIN users u ON u.id = m.user_id WHERE m.project_id = ?1 ORDER BY u.name",
            )?;
            let members = st
                .query_map([&pid], |r| {
                    Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                               "email": r.get::<_, String>(2)?, "role": r.get::<_, String>(3)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((p, members))
        })
        .await?;
    project["members"] = json!(members);
    project["compilers"] = json!(crate::compile::available_compilers());
    Ok(Json(project))
}

#[derive(Deserialize)]
pub struct UpdateReq {
    name: Option<String>,
    compiler: Option<String>,
    #[serde(rename = "mainFile")]
    main_file: Option<String>,
}

pub async fn update(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<UpdateReq>,
) -> AppResult<Json<Value>> {
    let role = state.require(&id, user.id, Role::Editor).await?;
    let name = match req.name {
        Some(n) if role == Role::Owner => Some(clean_name(&n)?),
        Some(_) => return Err(AppError::forbidden()),
        None => None,
    };
    if let Some(c) = &req.compiler
        && !is_known_compiler(c)
    {
        return Err(AppError::bad("unknown compiler"));
    }
    let main = req.main_file.map(|m| clean_rel_path(&m)).transpose()?;
    let pid = id.clone();
    state
        .db(move |c| {
            if let Some(n) = name {
                c.execute(
                    "UPDATE projects SET name = ?2 WHERE id = ?1",
                    params![pid, n],
                )?;
            }
            if let Some(cp) = req.compiler {
                c.execute(
                    "UPDATE projects SET compiler = ?2 WHERE id = ?1",
                    params![pid, cp],
                )?;
            }
            if let Some(m) = main {
                c.execute(
                    "UPDATE projects SET main_file = ?2 WHERE id = ?1",
                    params![pid, m],
                )?;
            }
            Ok(())
        })
        .await?;
    if let Some(room) = state.hub.get(&id) {
        room.broadcast_json(json!({ "type": "project" }));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let role = state.role(&id, user.id).await?;
    let pid = id.clone();
    if role != Role::Owner {
        // Non-owners "delete" a shared project by leaving it.
        state
            .db(move |c| {
                c.execute(
                    "DELETE FROM members WHERE project_id = ?1 AND user_id = ?2",
                    params![pid, user.id],
                )
            })
            .await?;
        return Ok(Json(json!({ "ok": true, "left": true })));
    }
    state.hub.close_room(&id);
    state
        .db(move |c| c.execute("DELETE FROM projects WHERE id = ?1", [pid]))
        .await?;
    let _ = tokio::fs::remove_dir_all(state.cfg.project_dir(&id)).await;
    let _ = tokio::fs::remove_dir_all(state.cfg.project_build_dir(&id)).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct CopyReq {
    name: Option<String>,
}

pub async fn copy(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<CopyReq>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    flush_project(&state, &id).await?;
    let pid = id.clone();
    let (name, compiler, main): (String, String, String) = state
        .db(move |c| {
            c.query_row(
                "SELECT name, compiler, main_file FROM projects WHERE id = ?1",
                [pid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .await?;
    let new_name = clean_name(&req.name.unwrap_or_else(|| format!("{name} (copy)")))?;
    let new_id = insert_project(&state, &user, &new_name, &compiler, &main).await?;
    let src = state.cfg.project_dir(&id);
    let dst = state.cfg.project_dir(&new_id);
    let out = tokio::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&src)
        .arg(&dst)
        .output()
        .await?;
    if !out.status.success() {
        return Err(
            anyhow::anyhow!("copy failed: {}", String::from_utf8_lossy(&out.stderr)).into(),
        );
    }
    // Copy uncommitted working-tree state too, then re-init git config for the copy.
    let (s, d) = (src.clone(), dst.clone());
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        for (rel, full, meta) in crate::files::walk(&s) {
            let t = d.join(&rel);
            if meta.is_dir() {
                std::fs::create_dir_all(&t)?;
            } else {
                std::fs::copy(&full, &t)?;
            }
        }
        Ok(())
    })
    .await
    .map_err(anyhow::Error::from)??;
    crate::git::git(&dst, &["remote", "remove", "origin"])
        .await
        .ok();
    init_repo(&dst).await?;
    commit_all(&dst, None, "Copy project", Some(&user)).await?;
    Ok(Json(json!({ "id": new_id })))
}

#[derive(Deserialize)]
pub struct MemberReq {
    email: String,
    role: String,
}

pub async fn add_member(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<MemberReq>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Owner).await?;
    let role = match Role::parse(&req.role) {
        Some(r @ (Role::Viewer | Role::Editor)) => r,
        _ => return Err(AppError::bad("role must be editor or viewer")),
    };
    let email = req.email.trim().to_string();
    let pid = id.clone();
    let res: Option<i64> = state
        .db(move |c| {
            let uid: Option<i64> = c
                .query_row("SELECT id FROM users WHERE email = ?1", [&email], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(uid) = uid else { return Ok(None) };
            let owner: i64 =
                c.query_row("SELECT owner_id FROM projects WHERE id = ?1", [&pid], |r| {
                    r.get(0)
                })?;
            if owner == uid {
                return Ok(Some(-1));
            }
            c.execute(
                "INSERT INTO members (project_id, user_id, role) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, user_id) DO UPDATE SET role = excluded.role",
                params![pid, uid, role.as_str()],
            )?;
            Ok(Some(uid))
        })
        .await?;
    match res {
        None => Err(AppError::bad(
            "no user with that email — they need an account first",
        )),
        Some(-1) => Err(AppError::bad("that user owns the project")),
        Some(uid) => Ok(Json(json!({ "ok": true, "userId": uid }))),
    }
}

pub async fn remove_member(
    State(state): State<SharedState>,
    user: User,
    Path((id, uid)): Path<(String, i64)>,
) -> AppResult<Json<Value>> {
    if uid != user.id {
        state.require(&id, user.id, Role::Owner).await?;
    }
    state
        .db(move |c| {
            c.execute(
                "DELETE FROM members WHERE project_id = ?1 AND user_id = ?2",
                params![id, uid],
            )
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn chat_history(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let rows = state
        .db(move |c| {
            let mut st = c.prepare(
                "SELECT * FROM (SELECT c.id, c.user_id, u.name, c.body, c.created_at FROM chat c JOIN users u ON u.id = c.user_id
                 WHERE c.project_id = ?1 ORDER BY c.id DESC LIMIT 200) ORDER BY id",
            )?;
            st.query_map([id], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "userId": r.get::<_, i64>(1)?, "name": r.get::<_, String>(2)?,
                           "body": r.get::<_, String>(3)?, "createdAt": r.get::<_, i64>(4)? }))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!(rows)))
}
