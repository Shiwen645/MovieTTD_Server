// Account changes share the login lock so old credentials cannot create a session mid-update.
fn update_account(state: &ServerState, session: &str, input: &Value) -> Response {
    {
        let mut attempts = state.attempts.lock().unwrap();
        attempts.retain(|time|time.elapsed()<Duration::from_secs(60));
        if attempts.len()>=10 {return error(429,"rate_limited","认证尝试过于频繁，请一分钟后重试");}
        attempts.push(Instant::now());
    }
    let mut admin = state.admin.lock().unwrap();
    let current = input["currentPassword"].as_str().unwrap_or("");
    let Ok(hash) = PasswordHash::new(&admin.password_hash) else {
        return error(500, "auth_configuration", "服务端账号配置损坏");
    };
    if current.len() > 1024 || Argon2::default().verify_password(current.as_bytes(), &hash).is_err() {
        return error(400, "incorrect_current_password", "当前密码不正确");
    }
    let username = input["username"].as_str().unwrap_or(&admin.username).trim();
    let password = input["newPassword"].as_str().unwrap_or("");
    if username.is_empty() || username.chars().count() > 64 || username.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return error(400, "invalid_username", "账号须为 1–64 个字符，不能包含空白或控制字符");
    }
    if !password.is_empty() && (!(11..=128).contains(&password.chars().count()) || password.chars().any(char::is_control)) {
        return error(400, "invalid_password", "新密码须为 11–128 个字符，不能包含控制字符");
    }
    if username == admin.username && password.is_empty() {
        return error(400, "account_unchanged", "请修改账号或填写新密码");
    }
    let password_hash = if password.is_empty() {admin.password_hash.clone()} else {
        let salt = SaltString::generate(&mut OsRng);
        match Argon2::default().hash_password(password.as_bytes(), &salt) {
            Ok(hash) => hash.to_string(),
            Err(_) => return error(500, "password_hash_failed", "密码保存失败，请重试"),
        }
    };
    let updated = Admin {username: username.to_owned(), password_hash};
    if let Err(message) = save_account(state, &updated) {
        return error(500, "account_save_failed", message);
    }
    *admin = updated;
    // Keep this authenticated connection; revoke all other sessions immediately.
    state.sessions.lock().unwrap().retain(|key, _| key == session);
    // A first-run credential file must not advertise an obsolete password.
    let bootstrap = state.dir.join("initial-admin.json");
    if bootstrap.exists() {let _ = fs::remove_file(bootstrap);}
    ok(json!({"username":admin.username,"otherSessionsRevoked":true}))
}

fn save_account(state: &ServerState, account: &Admin) -> Result<(), String> {
    let target=state.dir.join("admin.json");
    let temporary=state.dir.join(format!(".admin-{}.tmp",uuid::Uuid::new_v4().simple()));
    let result=(||{
        let mut options=fs::OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        let mut file=options.open(&temporary).map_err(|e|e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(account).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        file.sync_all().map_err(|e|e.to_string())?;
        drop(file);
        fs::rename(&temporary,&target).map_err(|e|e.to_string())
    })();
    if result.is_err(){let _=fs::remove_file(&temporary);}
    result
}

fn storage_file_kind(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|v|v.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "strm" => "strm",
        "mp4"|"mkv"|"avi"|"mov"|"wmv"|"flv"|"m4v"|"ts"|"m2ts"|"webm"|"mpg"|"mpeg"|"iso" => "video",
        "ass"|"srt"|"ssa"|"sub"|"vtt"|"idx" => "subtitle",
        "jpg"|"jpeg"|"png"|"webp"|"gif"|"bmp"|"avif"|"svg" => "image",
        "nfo"|"txt"|"json"|"xml"|"cue"|"lrc"|"md"|"yaml"|"yml" => "text",
        _ => "other",
    }
}

static STORAGE_MUTATION_LOCK: Mutex<()> = Mutex::new(());
fn editable_file(path: &Path) -> bool {
    has_extension(
        path.to_str().unwrap_or(""),
        "strm,ass,srt,ssa,vtt,nfo,txt,json,xml,cue,lrc",
    )
}
fn valid_storage_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 240
        && name.trim() == name
        && !name.ends_with('.')
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !Regex::new(r"(?i)^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)")
            .unwrap()
            .is_match(name)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenamePreview {
    items: Vec<RenameItem>,
    valid: bool,
    token: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameItem {
    path: String,
    old_name: String,
    new_name: String,
    error: Option<String>,
    changed: bool,
}
fn rename_preview(paths: &[PathBuf], input: &Value) -> Result<RenamePreview, String> {
    if paths.is_empty() || paths.len() > 1000 {
        return Err("请选择 1–1000 个文件或文件夹".into());
    }
    let mode = input["mode"].as_str().unwrap_or("single");
    if mode == "single" && paths.len() != 1 {
        return Err("单项重命名只能选择一个项目".into());
    }
    let name = input["name"].as_str().unwrap_or("");
    let find = input["find"].as_str().unwrap_or("");
    let replace = input["replace"].as_str().unwrap_or("");
    if [name, find, replace].iter().any(|v| v.len() > 2048) {
        return Err("重命名规则过长".into());
    }
    let regex = if mode == "replace" && input["regex"].as_bool() == Some(true) {
        Some(
            regex::RegexBuilder::new(find)
                .case_insensitive(input["ignoreCase"].as_bool().unwrap_or(false))
                .size_limit(1024 * 1024)
                .build()
                .map_err(|e| format!("正则表达式无效：{e}"))?,
        )
    } else {
        None
    };
    let start = input["start"].as_u64().unwrap_or(1).min(999999);
    let padding = input["padding"].as_u64().unwrap_or(2).clamp(1, 8) as usize;
    let keep_ext = input["keepExtension"].as_bool().unwrap_or(true);
    let mut items = Vec::new();
    let mut signature = String::new();
    for (i, path) in paths.iter().enumerate() {
        let old = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("文件名无效")?;
        let ext = if path.is_file() && keep_ext && mode != "single" {
            path.extension().and_then(|v| v.to_str()).unwrap_or("")
        } else {
            ""
        };
        let stem = if ext.is_empty() {
            old
        } else {
            path.file_stem().and_then(|v| v.to_str()).unwrap_or(old)
        };
        let new = match mode {
            "single" => name.to_string(),
            "template" => {
                if !name.contains("{n}") && !name.contains("{name}") && paths.len() > 1 {
                    return Err("多个项目的命名模板应包含 {n} 或 {name}".into());
                }
                name.replace("{name}", stem)
                    .replace("{n}", &format!("{:0padding$}", start + i as u64))
            }
            "replace" => {
                if find.is_empty() {
                    return Err("请输入需要查找的文字".into());
                }
                if let Some(re) = &regex {
                    re.replace_all(stem, replace).into_owned()
                } else if input["ignoreCase"].as_bool() == Some(true) {
                    regex::RegexBuilder::new(&regex::escape(find))
                        .case_insensitive(true)
                        .build()
                        .unwrap()
                        .replace_all(stem, regex::NoExpand(replace))
                        .into_owned()
                } else {
                    stem.replace(find, replace)
                }
            }
            "affix" => format!(
                "{}{}{}",
                input["prefix"].as_str().unwrap_or(""),
                stem,
                input["suffix"].as_str().unwrap_or("")
            ),
            _ => return Err("未知重命名方式".into()),
        };
        let new = if ext.is_empty() {
            new
        } else {
            format!("{new}.{ext}")
        };
        let meta = fs::metadata(path).map_err(|_| "所选项目已不存在，请刷新目录")?;
        signature.push_str(&format!(
            "{}:{}:{:?}\n",
            path.display(),
            meta.len(),
            meta.modified().ok()
        ));
        items.push(RenameItem {
            path: path.to_string_lossy().into(),
            old_name: old.into(),
            changed: new != old,
            error: (!valid_storage_name(&new)).then(|| "名称包含无效字符或保留名称".into()),
            new_name: new,
        });
    }
    let key = |p: &Path| p.to_string_lossy().to_lowercase();
    for i in 0..items.len() {
        let to = paths[i].with_file_name(&items[i].new_name);
        let duplicate = (0..items.len())
            .any(|j| j != i && key(&paths[j].with_file_name(&items[j].new_name)) == key(&to));
        let occupied =
            fs::symlink_metadata(&to).is_ok() && !paths.iter().any(|p| key(p) == key(&to));
        let nested = paths
            .iter()
            .enumerate()
            .any(|(j, p)| i != j && paths[i].starts_with(p));
        if duplicate {
            items[i].error = Some("多个项目会产生相同名称".into());
        } else if occupied {
            items[i].error = Some("目标名称已存在".into());
        } else if nested {
            items[i].error = Some("不能同时重命名父目录与其内部项目".into());
        }
    }
    let valid = items.iter().all(|v| v.error.is_none());
    let token = token_hash(&format!(
        "{signature}{}",
        serde_json::to_string(&items).unwrap()
    ));
    Ok(RenamePreview {
        items,
        valid,
        token,
    })
}
fn apply_rename(state: &ServerState, paths: &[PathBuf], input: &Value) -> Result<Value, String> {
    let preview = rename_preview(paths, input)?;
    if !preview.valid {
        return Err("重命名存在冲突，请检查预览".into());
    }
    if input["previewToken"].as_str() != Some(&preview.token) {
        return Err("目录或规则已变化，请重新预览后确认".into());
    }
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for (path, item) in paths.iter().zip(&preview.items).filter(|(_, v)| v.changed) {
        let temp = path.with_file_name(format!(".rename-{}", uuid::Uuid::new_v4().simple()));
        if let Err(e) = fs::rename(path, &temp) {
            for (from, tmp, _) in staged.iter().rev() {
                let _ = fs::rename(tmp, from);
            }
            return Err(format!("准备重命名失败，已尝试还原：{e}"));
        }
        staged.push((path.clone(), temp, path.with_file_name(&item.new_name)));
    }
    let mut applied = 0;
    for (_, temp, to) in &staged {
        let result = if fs::symlink_metadata(to).is_ok() {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "目标被其他进程占用",
            ))
        } else {
            fs::rename(temp, to)
        };
        if let Err(e) = result {
            let mut restored = true;
            for (_, tmp, dest) in staged[..applied].iter().rev() {
                restored &= fs::rename(dest, tmp).is_ok();
            }
            for (from, tmp, _) in staged.iter().rev() {
                restored &= fs::rename(tmp, from).is_ok();
            }
            return Err(format!(
                "重命名失败：{e}；{}",
                if restored {
                    "已还原原名称"
                } else {
                    "部分还原失败，请检查 .rename 临时项目"
                }
            ));
        }
        applied += 1;
    }
    let moves: Vec<_> = staged
        .iter()
        .map(|(from, _, to)| (from.clone(), to.clone()))
        .collect();
    let warning = relocate_library_index(&state.dir, &moves).err();
    Ok(json!({"ok":true,"renamed":applied,"warning":warning}))
}

// Iterative traversal avoids recursion limits and never follows symbolic links.
fn storage_size(paths: &[PathBuf]) -> Result<Value, String> {
    if paths.is_empty() || paths.len()>1000 {return Err("请选择 1–1000 个项目".into());}
    let mut stack:Vec<PathBuf> = paths.iter().enumerate().filter(|(i,path)| !paths.iter().enumerate().any(|(j,parent)| j!=*i && (path.starts_with(parent) && (path!=&parent || j<*i)))).map(|(_,path)|path.clone()).collect();
    let mut size=0u64;
    let mut files=0u64;
    let mut directories=0u64;
    while let Some(path)=stack.pop() {
        let metadata=fs::symlink_metadata(&path).map_err(|e|format!("无法读取 {}：{e}",path.display()))?;
        if metadata.file_type().is_symlink(){continue;}
        if metadata.is_file(){size=size.checked_add(metadata.len()).ok_or("目录大小超出计算范围")?;files+=1;}
        else if metadata.is_dir(){
            directories+=1;
            for entry in fs::read_dir(&path).map_err(|e|format!("无法读取目录 {}：{e}",path.display()))? {
                stack.push(entry.map_err(|e|e.to_string())?.path());
            }
        }
    }
    Ok(json!({"size":size,"fileCount":files,"directoryCount":directories}))
}
