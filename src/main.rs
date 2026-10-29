use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload, rand_core::RngCore},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use chrono::{TimeZone, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const API_VERSION: u32 = 1;
const DEFAULT_API: &str = "https://api.themoviedb.org/3";
const DEFAULT_IMAGE: &str = "https://image.tmdb.org/t/p";
const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:18766";
const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/127.0.0.0 Safari/537.36";
static VAULT: OnceLock<Mutex<Vault>> = OnceLock::new();

include!("core.rs");
include!("quota.rs");
include!("dashboard.rs");
include!("storage_tools.rs");
include!("generation.rs");
include!("resources.rs");
include!("automation.rs");
include!("telegram.rs");
include!("drive_control.rs");

fn provider_url(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.into())
}
fn now() -> i64 {
    Utc::now().timestamp()
}
fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temp = path.with_extension("tmp");
    fs::write(
        &temp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())
}
fn protect_data_directory(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("whoami")
            .arg("/user")
            .arg("/fo")
            .arg("csv")
            .arg("/nh")
            .creation_flags(0x08000000)
            .output()
            .map_err(|_| "无法读取当前服务身份")?;
        let row = String::from_utf8_lossy(&output.stdout);
        let sid = row.trim().split(',').last().unwrap_or("").trim_matches('"');
        if !sid.starts_with("S-1-") {
            return Err("无法确认当前服务用户 SID".into());
        }
        let grant = format!("*{sid}:(OI)(CI)F");
        let status = std::process::Command::new("icacls")
            .arg(dir)
            .args(["/inheritance:r", "/grant:r", &grant])
            .creation_flags(0x08000000)
            .output()
            .map_err(|_| "无法限制服务数据目录权限")?;
        if !status.status.success() {
            return Err("无法限制服务数据目录 ACL".into());
        }
    }
    Ok(())
}
fn private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
    Ok(())
}
struct Vault {
    path: PathBuf,
    cipher: Aes256Gcm,
    values: HashMap<String, String>,
}
impl Vault {
    fn open(dir: &Path) -> Result<Self, String> {
        let key_path = dir.join("master.key");
        let key = if let Ok(value) = std::env::var("MOVIETTD_MASTER_KEY") {
            URL_SAFE
                .decode(value.trim())
                .map_err(|_| "MOVIETTD_MASTER_KEY 必须是 URL-safe base64")?
        } else if key_path.exists() {
            fs::read(&key_path).map_err(|e| e.to_string())?
        } else {
            let mut key = vec![0u8; 32];
            OsRng.fill_bytes(&mut key);
            fs::write(&key_path, &key).map_err(|e| e.to_string())?;
            private_file(&key_path)?;
            key
        };
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|_| "服务端加密主密钥必须为 32 字节")?;
        let path = dir.join("vault.json");
        let values = if path.exists() {
            serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("凭证库损坏：{e}"))?
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            cipher,
            values,
        })
    }
    fn get(&self, name: &str) -> Option<String> {
        let bytes = URL_SAFE.decode(self.values.get(name)?).ok()?;
        if bytes.len() < 12 {
            return None;
        }
        let plain = self
            .cipher
            .decrypt(
                Nonce::from_slice(&bytes[..12]),
                Payload {
                    msg: &bytes[12..],
                    aad: name.as_bytes(),
                },
            )
            .ok()?;
        String::from_utf8(plain).ok()
    }
    fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let previous = self.values.clone();
        if value.is_empty() {
            self.values.remove(name);
        } else {
            let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
            let encrypted = self
                .cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: value.as_bytes(),
                        aad: name.as_bytes(),
                    },
                )
                .map_err(|_| "加密凭证失败")?;
            let mut bytes = nonce.to_vec();
            bytes.extend(encrypted);
            self.values.insert(name.into(), URL_SAFE.encode(bytes));
        }
        if let Err(error) = atomic_json(&self.path, &self.values) {
            self.values = previous;
            return Err(error);
        }
        private_file(&self.path)
    }
}
fn secret(name: &str) -> Option<String> {
    VAULT.get()?.lock().ok()?.get(name)
}
fn store_secret(name: &str, value: &str) -> Result<(), String> {
    VAULT
        .get()
        .ok_or("服务凭证库未初始化")?
        .lock()
        .map_err(|_| "凭证库锁失败")?
        .set(name, value)
}

#[derive(Serialize, Deserialize)]
struct Admin {
    username: String,
    password_hash: String,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: String,
    status: String,
    processed: usize,
    total: usize,
    message: String,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    output_root: String,
    #[serde(default)]
    source_names: Vec<String>,
    min_size_mb: u64,
    #[serde(default)]
    strm_settings: StrmSettings,
    results: Vec<TaskResult>,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TaskResult {
    status: String,
    source: String,
    destination: Option<String>,
    message: String,
}
struct ServerState {
    server_id: String,
    data: Mutex<PersistedState>,
    dir: PathBuf,
    library_root: PathBuf,
    storage_roots: Mutex<Vec<PathBuf>>,
    task_submit: Mutex<()>,
    active_job: Mutex<Option<String>>,
    admin: Mutex<Admin>,
    sessions: Mutex<HashMap<String, i64>>,
    attempts: Mutex<Vec<Instant>>,
    jobs: Mutex<Vec<Task>>,
    job_write: Mutex<()>,
    config_write: Mutex<()>,
    automation: Mutex<AutomationData>,
    telegram: Mutex<Option<TelegramBridge>>,
}
fn storage_path(state: &ServerState, path: &str) -> Result<PathBuf, String> {
    let requested = Path::new(path);
    let roots = state.storage_roots.lock().map_err(|_| "保存目录锁失败")?;
    for root in roots.iter() {
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            root.join(requested)
        };
        let canonical = fs::canonicalize(&candidate).map_err(|_| "文件不存在或无权访问")?;
        if canonical.starts_with(root)
            && !canonical
                .symlink_metadata()
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Ok(canonical);
        }
    }
    Err("路径不在已登记的保存目录内".into())
}
fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none_or(|parent| parent == path)
}

fn persist(state: &ServerState) -> Result<(), String> {
    let _lock = state.config_write.lock().map_err(|_| "配置写入锁失败")?;
    let data = state.data.lock().map_err(|_| "状态锁失败")?;
    atomic_json(&state.dir.join("settings.json"), &*data)
}
fn persist_jobs(state: &ServerState) -> Result<(), String> {
    let _lock = state.job_write.lock().map_err(|_| "任务写入锁失败")?;
    let jobs = state.jobs.lock().map_err(|_| "任务锁失败")?;
    atomic_json(&state.dir.join("tasks.json"), &*jobs)
}
fn update_task(
    state: &ServerState,
    id: &str,
    change: impl FnOnce(&mut Task),
) -> Result<(), String> {
    {
        let mut jobs = state.jobs.lock().map_err(|_| "任务锁失败")?;
        let task = jobs.iter_mut().find(|j| j.id == id).ok_or("任务不存在")?;
        change(task);
        task.updated_at = now();
    }
    persist_jobs(state)
}
fn cancelled(state: &ServerState, id: &str) -> bool {
    state
        .jobs
        .lock()
        .map(|jobs| {
            jobs.iter()
                .find(|j| j.id == id)
                .is_none_or(|j| j.status == "cancelled")
        })
        .unwrap_or(true)
}
fn task_log(state: &ServerState, id: &str, result: TaskResult) -> Result<(), String> {
    let record = json!({"taskId":id,"time":Utc::now().to_rfc3339(),"result":result});
    let _lock = state.job_write.lock().map_err(|_| "日志写入锁失败")?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.dir.join("import-history.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(file, "{record}").map_err(|e| e.to_string())
}
fn safe_library_path(state: &ServerState, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.components().any(|part| {
        !matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err("目录必须位于服务端媒体库根目录内".into());
    }
    let target =
        fs::canonicalize(state.library_root.join(path)).map_err(|_| "目录不存在或无权访问")?;
    if !target.starts_with(&state.library_root) || !target.is_dir() {
        return Err("目录超出允许的媒体库根目录".into());
    }
    Ok(target)
}
fn create_storage_dir(root: &Path, destination: &Path) -> Result<PathBuf, String> {
    if !destination.starts_with(root) {
        return Err("目标目录超出已登记保存根目录".into());
    }
    let relative = destination.strip_prefix(root).map_err(|_| "目标目录无效")?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        let std::path::Component::Normal(name) = part else {
            return Err("媒体目录路径无效".into());
        };
        current.push(name);
        if !current.exists() {
            fs::create_dir(&current).map_err(|e| e.to_string())?;
        }
        let canonical = fs::canonicalize(&current).map_err(|e| e.to_string())?;
        if !canonical.starts_with(root) || !canonical.is_dir() {
            return Err("目标目录包含越界链接".into());
        }
        current = canonical;
    }
    Ok(current)
}
fn copy_storage_tree(source: &Path, target: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Err("不复制符号链接".into());
    }
    if meta.is_file() {
        fs::copy(source, target).map_err(|e| e.to_string())?;
        return Ok(());
    }
    fs::create_dir(target).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue;
        }
        copy_storage_tree(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}
fn run_task(state: Arc<ServerState>, id: String, url: String, password: String) {
    let result = (|| -> Result<(), String> {
        update_task(&state, &id, |t| {
            if t.status != "cancelled" {
                t.status = "running".into();
                t.message = "正在读取分享目录".into();
            }
        })?;
        if cancelled(&state, &id) {
            return Ok(());
        }
        let task = state
            .jobs
            .lock()
            .map_err(|_| "任务锁失败")?
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or("任务不存在")?;
        let root = if Path::new(&task.output_root).is_absolute() {
            storage_path(&state, &task.output_root)?
        } else {
            safe_library_path(&state, &task.output_root)?
        };
        let root_guard = state
            .storage_roots
            .lock()
            .unwrap()
            .iter()
            .filter(|p| root.starts_with(p.as_path()))
            .max_by_key(|p| p.components().count())
            .cloned()
            .unwrap_or_else(|| state.library_root.clone());
        let key = share_key(&url)?;
        let password = extract_password(&url, &password);
        let (mut settings, unclassified, proxy_base) = {
            let data = state.data.lock().map_err(|_| "状态锁失败")?;
            (
                data.tmdb.clone().unwrap_or_else(defaults),
                data.unclassified_folder.clone(),
                data.proxy_base_url.clone(),
            )
        };
        settings.api_key = secret("tmdb-api-key").unwrap_or_default();
        let mut queue = VecDeque::from([(String::from("0"), PathBuf::new())]);
        let mut seen = std::collections::HashSet::new();
        let mut videos = Vec::new();
        let mut attachments = Vec::new();
        while let Some((parent, rel)) = queue.pop_front() {
            if cancelled(&state, &id) {
                return Ok(());
            }
            if !seen.insert(parent.clone()) {
                continue;
            }
            if seen.len() > 10000 {
                return Err("分享目录超过安全遍历上限".into());
            }
            let files = list_share(&key, &password, &parent)?;
            if parent == "0" {
                let source_names = share_source_names(&files);
                update_task(&state, &id, |task| task.source_names = source_names)?;
            }
            for file in files {
                if Path::new(&file.name).components().count() != 1
                    || file.name.contains(['\\', '/'])
                    || matches!(file.name.as_str(), "." | "..")
                {
                    return Err("分享包含无效文件名".into());
                }
                let child = rel.join(&file.name);
                if file.is_dir {
                    queue.push_back((file.id, child));
                } else if has_extension(&file.name, &task.strm_settings.video_extensions)
                    && file.size >= task.min_size_mb * 1024 * 1024
                {
                    videos.push((file, child));
                } else if has_extension(&file.name, &task.strm_settings.metadata_extensions) {
                    attachments.push((file, child));
                } else {
                    let entry = TaskResult {
                        status: "skipped".into(),
                        source: child.to_string_lossy().into(),
                        destination: None,
                        message: "非视频或小于最小文件大小".into(),
                    };
                    task_log(&state, &id, entry.clone())?;
                    update_task(&state, &id, |t| t.results.push(entry))?;
                }
            }
        }
        update_task(&state, &id, |t| {
            t.total = videos.len() + attachments.len();
            t.message = format!("发现 {} 个符合条件的视频", videos.len());
        })?;
        let proxy_ids = register_proxy_files(&state, &key, &password, &videos)?;
        let mut cache = HashMap::new();
        let mut generated = Vec::new();
        let mut destinations = HashMap::new();
        let mut skipped_sources = Vec::new();
        for (index, (file, rel)) in videos.iter().enumerate() {
            if cancelled(&state, &id) {
                return Ok(());
            }
            let parsed = import_media_name(&file.name, rel);
            let cache_key = format!(
                "{}\n{}\n{}\n{}",
                parsed.kind,
                parsed.title,
                parsed.year,
                tmdb_id_from_path(&rel).unwrap_or_default()
            );
            let metadata = cache
                .entry(cache_key)
                .or_insert_with(|| tmdb_match_for_import(&parsed, &rel, &settings));
            let result = (|| -> Result<(PathBuf, bool, String), String> {
                let epname = match (
                    metadata.as_ref().and_then(|v| v["id"].as_u64()),
                    parsed.episode,
                ) {
                    (Some(tmdb), Some(episode)) if parsed.kind == "tv" => {
                        episode_title(tmdb, parsed.season.unwrap_or(1), episode, &settings)
                    }
                    _ => String::new(),
                };
                let dest =
                    library_destination_dir(&root, &parsed, metadata.as_ref(), &unclassified);
                let dest = create_storage_dir(&root_guard, &dest)?;
                let stem = library_file_stem(&parsed, metadata.as_ref(), &epname, &file.name);
                let lookup = format!("{}\n{}", file.id, rel.to_string_lossy().replace('\\', "/"));
                let proxy_id = proxy_ids.get(&lookup).ok_or("代理映射缺失")?;
                let link = format!("{}/media/{proxy_id}", proxy_base.trim_end_matches('/'));
                let out = dest.join(format!("{stem}.strm"));
                let write = write_resource(&state, &root, &out, &link, file, &parsed, metadata.as_ref(), &task.strm_settings.duplicate_policy)?;
                let out = write.path;
                generated.push(json!({"path":out,"link":write.link,"preserveOnly":write.preserve_only}));
                if write.written {
                    record_library_item_in(&state.dir, &out, &parsed, metadata.as_ref())?;
                    destinations.insert(rel.clone(), out.clone());
                } else {
                    skipped_sources.push(rel.clone());
                }
                Ok((out, write.written, write.reason))
            })();
            let (category, reason) = library_category(metadata.as_ref());
            let entry = match result {
                Ok((out, written, duplicate_reason)) => TaskResult {
                    status: if written { "success" } else { "skipped" }.into(),
                    source: rel.to_string_lossy().into(),
                    destination: Some(out.to_string_lossy().into()),
                    message: format!(
                        "{}：{reason}；{duplicate_reason}",
                        if category.is_empty() {
                            &unclassified
                        } else {
                            category
                        }
                    ),
                },
                Err(error) => TaskResult {
                    status: "failed".into(),
                    source: rel.to_string_lossy().into(),
                    destination: None,
                    message: error,
                },
            };
            task_log(&state, &id, entry.clone())?;
            update_task(&state, &id, |t| {
                t.processed = index + 1;
                t.message = format!("已处理 {}/{}", index + 1, t.total);
                t.results.push(entry);
            })?;
        }
        for (index, (file, rel)) in attachments.iter().enumerate() {
            if cancelled(&state, &id) {
                return Ok(());
            }
            let skipped_video = skipped_sources.iter().any(|source| metadata_belongs_to(rel, source));
            if skipped_video {
                let entry = TaskResult { status: "skipped".into(), source: rel.to_string_lossy().into(), destination: None, message: "对应视频未替换，保留已有附属文件".into() };
                update_task(&state, &id, |t| { t.processed = videos.len()+index+1; t.results.push(entry); })?;
                continue;
            }
            let result = download_metadata(
                &state,
                &key,
                file,
                rel,
                &root,
                &root_guard,
                &unclassified,
                &destinations,
                &settings,
            );
            let entry = match result {
                Ok((path, exists)) => TaskResult {
                    status: if exists { "skipped" } else { "success" }.into(),
                    source: rel.to_string_lossy().into(),
                    destination: Some(path.to_string_lossy().into()),
                    message: if exists {
                        "已有元数据，保留原文件"
                    } else {
                        "已下载元数据"
                    }
                    .into(),
                },
                Err(e) => TaskResult {
                    status: "failed".into(),
                    source: rel.to_string_lossy().into(),
                    destination: None,
                    message: e,
                },
            };
            task_log(&state, &id, entry.clone())?;
            update_task(&state, &id, |t| {
                t.results.push(entry);
                t.processed = videos.len() + index + 1;
            })?;
        }
        if !cancelled(&state, &id) {
            // Only reconcile after a complete, successful traversal and processing pass.
            let no_failures = state
                .jobs
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == id)
                .is_some_and(|t| !t.results.iter().any(|r| r.status == "failed"));
            if no_failures {
                reconcile_generated(&state, &key, &root, &generated, task.strm_settings.duplicate_policy=="keep")?;
            }
        }
        update_task(&state, &id, |t| {
            if t.status != "cancelled" {
                let failures = t.results.iter().filter(|r| r.status == "failed").count();
                t.status = if failures > 0 { "partial" } else { "completed" }.into();
                t.message = format!(
                    "任务完成：成功 {}，跳过 {}，失败 {failures}",
                    t.results.iter().filter(|r| r.status == "success").count(),
                    t.results.iter().filter(|r| r.status == "skipped").count()
                );
            }
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = update_task(&state, &id, |t| {
            if t.status != "cancelled" {
                t.status = "failed".into();
                t.message = "导入失败".into();
                t.error = Some(error);
            }
        });
    }
}
fn start_task(state: &Arc<ServerState>, input: &Value) -> Result<Value, String> {
    let _submit = state.task_submit.lock().map_err(|_| "任务提交锁失败")?;
    if state.active_job.lock().map_err(|_| "任务锁失败")?.is_some() {
        return Err("上一个任务尚未停止，请稍后再提交".into());
    }
    let url = input["url"].as_str().ok_or("缺少分享链接")?.to_string();
    share_key(&url)?;
    let password = input["password"].as_str().unwrap_or("").to_string();
    let relative = input["outputRoot"].as_str().unwrap_or("");
    let path = if Path::new(relative).is_absolute() {
        storage_path(state, relative)?
    } else {
        safe_library_path(state, relative)?
    };
    let mut strm_settings = if let Some(config) = input.get("strmSettings") {
        validated_strm_settings(config)?
    } else {
        state.data.lock().map_err(|_| "配置锁失败")?.strm.clone()
    };
    let min_size = input["minSizeMb"]
        .as_u64()
        .unwrap_or(strm_settings.min_size_mb);
    strm_settings.min_size_mb = min_size;
    if min_size > 10240 {
        return Err("最小文件大小不能超过 10240MB".into());
    }
    if secret("drive-pan123").is_none() {
        return Err("请先绑定有效的 123 云盘登录态".into());
    }
    if state
        .jobs
        .lock()
        .map_err(|_| "任务锁失败")?
        .iter()
        .any(|j| matches!(j.status.as_str(), "queued" | "running"))
    {
        return Err("已有导入任务运行中，请等待或取消".into());
    }
    let probe = path.join(format!(".moviettd-check-{}", uuid::Uuid::new_v4()));
    fs::write(&probe, []).map_err(|_| "服务端目录不可写")?;
    fs::remove_file(&probe).map_err(|e| e.to_string())?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    store_secret(
        &format!("task-{id}"),
        &json!({"url":url,"password":password}).to_string(),
    )?;
    let task = Task {
        id: id.clone(),
        status: "queued".into(),
        processed: 0,
        total: 0,
        message: "任务已排队".into(),
        error: None,
        created_at: now(),
        updated_at: now(),
        output_root: relative.into(),
        source_names: vec![],
        min_size_mb: min_size,
        strm_settings,
        results: vec![],
    };
    state
        .jobs
        .lock()
        .map_err(|_| "任务锁失败")?
        .push(task.clone());
    persist_jobs(state)?;
    *state.active_job.lock().map_err(|_| "任务锁失败")? = Some(id.clone());
    let service = state.clone();
    thread::spawn(move || {
        let worker = service.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_task(worker, id.clone(), url, password)
        }));
        if result.is_err() {
            let _ = update_task(&service, &id, |t| {
                t.status = "failed".into();
                t.message = "任务处理异常，请查看服务日志".into();
            });
        }
        *service.active_job.lock().unwrap() = None;
    });
    serde_json::to_value(task).map_err(|e| e.to_string())
}

fn error(status: u16, code: &str, message: impl Into<String>) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"error":{"code":code,"message":message.into()}})),
    )
        .into_response()
}
fn ok(value: Value) -> Response {
    Json(value).into_response()
}
fn health(state: &ServerState) -> Response {
    ok(
        json!({"service":"moviettd-server","status":"ok","apiVersion":API_VERSION,"version":env!("CARGO_PKG_VERSION"),"serverId":state.server_id,"authRequired":true}),
    )
}
fn authorized(state: &ServerState, headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let hash = token_hash(token);
    state
        .sessions
        .lock()
        .ok()?
        .get(&hash)
        .filter(|expires| **expires > now())?;
    Some(hash)
}
fn safe_public_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value.trim()).map_err(|_| "地址格式错误")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("地址必须是无账号、密码、查询参数的 HTTP(S) URL".into());
    }
    Ok(value.trim().trim_end_matches('/').into())
}
fn dispatch(
    state: Arc<ServerState>,
    method: Method,
    target: String,
    headers: HeaderMap,
    body: Vec<u8>,
) -> Response {
    let url = match reqwest::Url::parse(&format!("http://localhost{target}")) {
        Ok(u) => u,
        Err(_) => return error(400, "invalid_request", "请求路径无效"),
    };
    let path = url.path();
    if matches!(path, "/health" | "/api/v1/handshake")
        && matches!(method, Method::GET | Method::HEAD)
    {
        return health(&state);
    }
    if path.starts_with("/media/") || path.starts_with("/123pan/") {
        if !matches!(method, Method::GET | Method::HEAD) {
            return error(405, "method_not_allowed", "仅支持 GET/HEAD");
        }
        return match proxy_lookup(&state, &target) {
            Ok(ProxyReply::Json(bytes)) => (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                bytes,
            )
                .into_response(),
            Ok(ProxyReply::Redirect(location)) => {
                let parsed = reqwest::Url::parse(&location);
                if location.contains(['\r', '\n'])
                    || !parsed.is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
                {
                    return error(502, "invalid_upstream", "上游返回无效直链");
                }
                match header::HeaderValue::from_str(&location) {
                    Ok(location) => (
                        StatusCode::FOUND,
                        [
                            (header::LOCATION, location),
                            (
                                header::CACHE_CONTROL,
                                header::HeaderValue::from_static("no-store"),
                            ),
                        ],
                    )
                        .into_response(),
                    Err(_) => error(502, "invalid_upstream", "上游返回无效直链"),
                }
            }
            Err((status, message)) => error(status, "media_proxy_error", message),
        };
    }
    let input: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => return error(400, "invalid_json", "请求必须为 JSON"),
        }
    };
    if path == "/api/v1/auth/login" && method == Method::POST {
        {
            let mut attempts = state.attempts.lock().unwrap();
            attempts.retain(|time| time.elapsed() < Duration::from_secs(60));
            if attempts.len() >= 10 {
                return error(429, "rate_limited", "登录尝试过于频繁，请一分钟后重试");
            }
            attempts.push(Instant::now());
        }
        let admin = state.admin.lock().unwrap();
        let hash = match PasswordHash::new(&admin.password_hash) {
            Ok(h) => h,
            Err(_) => return error(500, "auth_configuration", "服务端账号配置损坏"),
        };
        let valid = Argon2::default()
            .verify_password(input["password"].as_str().unwrap_or("").as_bytes(), &hash)
            .is_ok();
        if input["username"].as_str() != Some(&admin.username) || !valid {
            return error(401, "invalid_credentials", "用户名或密码错误");
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires = now() + 7 * 86400;
        let mut sessions = state.sessions.lock().unwrap();
        sessions.retain(|_, expiry| *expiry > now());
        sessions.insert(token_hash(&token), expires);
        return ok(
            json!({"accessToken":token,"expiresAt":expires,"username":admin.username}),
        );
    }
    let Some(session) = authorized(&state, &headers) else {
        return error(401, "unauthorized", "服务器登录已过期或尚未登录");
    };
    if path == "/api/v1/auth/me" && method == Method::GET {
        let admin = state.admin.lock().unwrap();
        return ok(json!({"username":admin.username}));
    }
    if path == "/api/v1/auth/account" && method == Method::PUT {
        return update_account(&state, &session, &input);
    }
    if path == "/api/v1/auth/logout" && method == Method::POST {
        state.sessions.lock().unwrap().remove(&session);
        return ok(json!({"ok":true}));
    }
    if path == "/api/v1/dashboard" && method == Method::GET {
        return match dashboard(&state) {
            Ok(value) => ok(value),
            Err(message) => error(500, "library_scan_failed", message),
        };
    }
    if path == "/api/v1/storage/roots" {
        if method == Method::GET {
            let roots = state.storage_roots.lock().unwrap().clone();
            return ok(
                json!({"roots":roots.iter().map(|p|json!({"path":p,"label":p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()})).collect::<Vec<_>>() }),
            );
        }
        if method == Method::POST {
            let requested = Path::new(input["path"].as_str().unwrap_or(""));
            let root = match fs::canonicalize(requested) {
                Ok(v) if v.is_dir() => v,
                _ => return error(400, "invalid_storage_root", "保存目录不存在或不可访问"),
            };
            if is_filesystem_root(&root) {
                return error(
                    400,
                    "invalid_storage_root",
                    "请选择系统根目录下的具体文件夹，不能将整个磁盘作为媒体库",
                );
            }
            let mut roots = state.storage_roots.lock().unwrap();
            if !roots.contains(&root) {
                roots.push(root.clone());
            }
            if let Err(e) = atomic_json(&state.dir.join("storage-roots.json"), &*roots) {
                return error(500, "storage_roots_write_failed", e);
            }
            return ok(json!({"path":root,"ok":true}));
        }
        return error(405, "method_not_allowed", "保存目录接口支持 GET/POST");
    }
    if path == "/api/v1/storage/system" && method == Method::GET {
        let requested = url
            .query_pairs()
            .find(|(k, _)| k == "path")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        #[cfg(windows)]
        let roots: Vec<PathBuf> = ('A'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .filter(|p| p.is_dir())
            .collect();
        #[cfg(not(windows))]
        let roots: Vec<PathBuf> = vec![PathBuf::from("/")];
        if requested.is_empty() {
            let root_entries: Vec<Value> = roots
                .iter()
                .map(|root| json!({"name":root.to_string_lossy(),"path":root}))
                .collect();
            return ok(json!({"path":"","roots":root_entries,"directories":[],"selectable":false}));
        }
        let dir = if requested.is_empty() {
            None
        } else {
            match fs::canonicalize(&requested) {
                Ok(p) if p.is_dir() => Some(p),
                _ => return error(400, "invalid_system_path", "系统目录不存在或不可访问"),
            }
        };
        let path_value = dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let candidates = dir.as_ref().map(|p| vec![p.clone()]).unwrap_or(roots);
        let mut entries = Vec::new();
        for base in candidates {
            if let Ok(items) = fs::read_dir(&base) {
                for item in items.flatten() {
                    if item.file_type().is_ok_and(|t| t.is_dir()) {
                        let p = item.path();
                        if fs::canonicalize(&p).is_ok_and(|c| c.starts_with(&base)) {
                            entries.push(json!({"name":item.file_name().to_string_lossy(),"path":p.to_string_lossy()}));
                        }
                    }
                }
            }
        }
        entries.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_lowercase());
        return ok(
            json!({"path":path_value,"roots":Vec::<Value>::new(),"directories":entries,"selectable":dir.as_ref().is_some_and(|p|!is_filesystem_root(p))}),
        );
    }
    if path == "/api/v1/storage/browse" && method == Method::GET {
        let requested = url
            .query_pairs()
            .find(|(k, _)| k == "path")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let filter = url.query_pairs().find(|(key,_)|key=="type").map(|(_,value)|value.into_owned()).unwrap_or_else(||"all".into());
        if !["all","folder","strm","video","subtitle","image","text","other"].contains(&filter.as_str()) {
            return error(400,"invalid_file_filter","不支持的文件类型筛选");
        }
        let dir = match storage_path(&state, &requested) {
            Ok(v) => v,
            Err(e) => return error(400, "invalid_file_path", e),
        };
        let mut entries = Vec::new();
        let mut total_count = 0usize;
        let mut type_counts = HashMap::<String, usize>::new();
        match fs::read_dir(&dir) {
            Ok(items) => {
                for item in items.flatten() {
                    let path = item.path();
                    let Ok(kind) = item.file_type() else { continue };
                    if kind.is_symlink() {
                        continue;
                    }
                    total_count += 1;
                    let file_kind = if kind.is_dir() {"folder"} else {storage_file_kind(&path)};
                    *type_counts.entry(file_kind.into()).or_default() += 1;
                    // Keep directories navigable when a particular file format is selected.
                    if filter != "all" && !kind.is_dir() && filter != file_kind {continue;}
                    let size = if kind.is_file() {
                        item.metadata().map(|m| m.len()).unwrap_or(0)
                    } else {
                        0
                    };
                    let modified = item
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    entries.push(json!({"name":item.file_name().to_string_lossy(),"path":path,"isDirectory":kind.is_dir(),"fileType":file_kind,"size":size,"modified":modified}));
                }
            }
            Err(e) => return error(403, "directory_access_denied", e.to_string()),
        };
        entries.sort_by(|a, b| {
            b["isDirectory"]
                .as_bool()
                .cmp(&a["isDirectory"].as_bool())
                .then_with(|| {
                    a["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
                })
        });
        return ok(json!({"path":dir,"entries":entries,"totalCount":total_count,"typeCounts":type_counts,"filter":filter}));
    }
    if path == "/api/v1/storage/file" && method == Method::GET {
        let requested = url
            .query_pairs()
            .find(|(k, _)| k == "path")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let file = match storage_path(&state, &requested) {
            Ok(v) => v,
            Err(e) => return error(400, "invalid_file_path", e),
        };
        if !file.is_file() || !editable_file(&file) {
            return error(400, "unsupported_file", "此文件格式不支持文本编辑");
        }
        if fs::metadata(&file).is_ok_and(|m| m.len() > 1024 * 1024) {
            return error(413, "file_too_large", "文本文件超过 1 MB");
        }
        use std::io::Read;
        let content = fs::File::open(file).and_then(|f| {
            let mut text = String::new();
            f.take(1024 * 1024 + 1).read_to_string(&mut text)?;
            Ok(text)
        });
        return match content {
            Ok(text) if text.len() <= 1024 * 1024 => ok(json!({"content":text})),
            Ok(_) => error(413, "file_too_large", "文本文件超过 1 MB"),
            Err(_) => error(
                400,
                "file_read_failed",
                "文件不是有效的 UTF-8 文本或无法读取",
            ),
        };
    }

    if path == "/api/v1/storage/files" && method == Method::POST {
        let _storage_guard = STORAGE_MUTATION_LOCK.lock().unwrap();
        let action = input["action"].as_str().unwrap_or("");
        let base = match storage_path(&state, input["path"].as_str().unwrap_or("")) {
            Ok(v) => v,
            Err(e) => return error(400, "invalid_file_path", e),
        };
        let paths: Vec<PathBuf> = input["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|p| storage_path(&state, p))
            .collect::<Result<_, _>>()
            .unwrap_or_else(|_| Vec::new());
        if input["paths"]
            .as_array()
            .is_some_and(|a| a.len() != paths.len())
        {
            return error(400, "invalid_file_path", "部分所选文件不在已登记保存目录内");
        }
        if paths
            .iter()
            .any(|selected| state.storage_roots.lock().unwrap().contains(selected))
        {
            return error(
                400,
                "storage_root_immutable",
                "不能移动或删除已登记的保存根目录",
            );
        }
        let name = input["name"].as_str().unwrap_or("").trim();
        let safe_name = valid_storage_name;
        let result: Result<Value, String> =
            (|| match action {
                "mkdir" | "create-file" => {
                    if !safe_name(name) {
                        return Err("文件名无效".into());
                    }
                    let target = base.join(name);
                    if action == "mkdir" {
                        fs::create_dir(target).map_err(|e| e.to_string())?
                    } else {
                        fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(target)
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(json!({"ok":true}))
                }
                "write" => {
                    let p = storage_path(&state, input["filePath"].as_str().unwrap_or(""))?;
                    if !editable_file(&p) {
                        return Err("此文件格式不支持文本编辑".into());
                    }
                    let content = input["content"].as_str().unwrap_or("");
                    if content.len() > 1024 * 1024 {
                        return Err("STRM 内容超过 1MB".into());
                    }
                    fs::write(p, content).map_err(|e| e.to_string())?;
                    Ok(json!({"ok":true}))
                }
                "upload" => {
                    if !safe_name(name) {
                        return Err("文件名无效".into());
                    }
                    let encoded = input["content"].as_str().unwrap_or("");
                    if encoded.len() > 11 * 1024 * 1024 {
                        return Err("单个上传文件不能超过 8 MB".into());
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|_| "上传内容编码无效")?;
                    let target = base.join(name);
                    if target.exists() {
                        return Err("目标文件已存在".into());
                    }
                    fs::write(target, bytes).map_err(|e| e.to_string())?;
                    Ok(json!({"ok":true}))
                }
                "delete" => {
                    for p in &paths {
                        if p.is_dir() {
                            fs::remove_dir_all(p).map_err(|e| e.to_string())?
                        } else {
                            fs::remove_file(p).map_err(|e| e.to_string())?
                        }
                    }
                    Ok(json!({"ok":true}))
                }
                "move" | "copy" => {
                    let dest = storage_path(&state, input["destination"].as_str().unwrap_or(""))?;
                    if !dest.is_dir() {
                        return Err("目标必须是文件夹".into());
                    }
                    for p in &paths {
                        if p.is_dir() && dest.starts_with(p) {
                            return Err("不能将文件夹复制或移动到自身内部".into());
                        }
                        let to = dest.join(p.file_name().ok_or("文件名无效")?);
                        if to.exists() {
                            return Err(format!("目标已存在：{}", to.display()));
                        }
                        if action == "move" {
                            match fs::rename(p, &to) {
                                Ok(()) => {}
                                Err(_) if p.is_dir() => {
                                    copy_storage_tree(p, &to)?;
                                    fs::remove_dir_all(p).map_err(|e| e.to_string())?
                                }
                                Err(e) => return Err(e.to_string()),
                            }
                        } else {
                            copy_storage_tree(p, &to)?;
                        }
                        if action == "move" {
                            relocate_library_index(&state.dir, &[(p.clone(), to)])?;
                        }
                    }
                    Ok(json!({"ok":true}))
                }
                "rename-preview" => Ok(serde_json::to_value(rename_preview(&paths, &input)?)
                    .map_err(|e| e.to_string())?),
                "rename" => apply_rename(&state, &paths, &input),
                "size" => storage_size(&paths),
                _ => Err("不支持的文件操作".into()),
            })();
        return match result {
            Ok(value) => ok(value),
            Err(e) => error(400, "file_operation_failed", e),
        };
    }
    if path.starts_with("/api/v1/connections/") && path.ends_with("/quota") && method == Method::GET
    {
        let provider = path
            .trim_start_matches("/api/v1/connections/")
            .trim_end_matches("/quota");
        return match drive_quota(provider) {
            Ok(value) => ok(value),
            Err(message) => error(502, "quota_unavailable", message),
        };
    }
    if path == "/api/v1/connections" && method == Method::GET {
        return ok(serde_json::to_value(&state.data.lock().unwrap().connections).unwrap());
    }
    if path == "/api/v1/connections/credentials" && method == Method::POST {
        let provider = input["provider"].as_str().unwrap_or("");
        let credential = input["credential"].as_str().unwrap_or("").trim();
        if credential.len() < 20
            || credential.len() > 32768
            || credential.chars().any(char::is_control)
        {
            return error(400, "invalid_credential", "网盘凭证格式无效");
        }
        let valid = match provider {
            "pan123" => validate_123_token(credential),
            "quark" | "uc" => validate_cookie(provider, credential),
            _ => Err("不支持该网盘".into()),
        };
        if let Err(message) = valid {
            return error(422, "drive_validation_failed", message);
        }
        if let Err(message) = store_secret(&format!("drive-{provider}"), credential) {
            return error(500, "vault_write_failed", message);
        }
        let label = match provider {
            "pan123" => "123 云盘",
            "quark" => "夸克网盘",
            _ => "UC 网盘",
        };
        {
            let mut data = state.data.lock().unwrap();
            data.connections.retain(|c| c.provider != provider);
            data.connections.push(Connection {
                provider: provider.into(),
                label: label.into(),
                connected: true,
                quota: None,
            });
        }
        if let Err(message) = persist(&state) {
            return error(500, "settings_write_failed", message);
        }
        return ok(json!({"provider":provider,"connected":true}));
    }
    if path.starts_with("/api/v1/automation") {
        return automation_api(&state, &method, &path, &input);
    }
    if path.starts_with("/api/v1/telegram") {
        return telegram_api(&state, &method, &path, &input);
    }
    if path == "/api/v1/strm/settings" {
        if method == Method::GET {
            return ok(json!(state.data.lock().unwrap().strm));
        }
        if method == Method::PUT {
            let value = match validated_strm_settings(&input) {
                Ok(v) => v,
                Err(e) => return error(400, "invalid_strm_settings", e),
            };
            let previous = state.data.lock().unwrap().strm.clone();
            state.data.lock().unwrap().strm = value;
            return match persist(&state) {
                Ok(()) => ok(json!(state.data.lock().unwrap().strm)),
                Err(e) => {
                    state.data.lock().unwrap().strm = previous;
                    error(500, "settings_write_failed", e)
                }
            };
        }
        return error(405, "method_not_allowed", "支持 GET/PUT");
    }
    if path == "/api/v1/settings" {
        if method == Method::GET {
            let data = state.data.lock().unwrap();
            let mut tmdb = data.tmdb.clone().unwrap_or_else(defaults);
            // This endpoint is authenticated and marked no-store. Persist secrets only in the vault.
            tmdb.api_key = secret("tmdb-api-key").unwrap_or_default();
            tmdb.api_key_configured = !tmdb.api_key.is_empty();
            return ok(
                json!({"tmdb":tmdb,"strm":data.strm,"proxyBaseUrl":data.proxy_base_url,"unclassifiedFolder":data.unclassified_folder,"libraryRoot":state.library_root,"proxyStatus":"服务端播放代理运行中"}),
            );
        }
        if method == Method::PUT {
            if let Some(value) = input.get("tmdb") {
                let base = match safe_public_url(value["baseUrl"].as_str().unwrap_or(DEFAULT_API)) {
                    Ok(v) => v,
                    Err(e) => return error(400, "invalid_tmdb_url", e),
                };
                let images = match safe_public_url(
                    value["imageBaseUrl"].as_str().unwrap_or(DEFAULT_IMAGE),
                ) {
                    Ok(v) => v,
                    Err(e) => return error(400, "invalid_image_url", e),
                };
                if let Some(key) = value["apiKey"].as_str().filter(|s| !s.trim().is_empty()) {
                    if let Err(e) = store_secret("tmdb-api-key", key.trim()) {
                        return error(500, "vault_write_failed", e);
                    }
                }
                state.data.lock().unwrap().tmdb = Some(TmdbSettings {
                    api_key: String::new(),
                    api_key_configured: secret("tmdb-api-key").is_some(),
                    base_url: base,
                    image_base_url: images,
                });
            }
            if let Some(value) = input["proxyBaseUrl"].as_str() {
                let value = match safe_public_url(value) {
                    Ok(v) => v,
                    Err(e) => return error(400, "invalid_proxy_url", e),
                };
                state.data.lock().unwrap().proxy_base_url = value;
            }
            if let Some(value) = input["unclassifiedFolder"].as_str() {
                let folder = match validate_unclassified_folder(value) {
                    Ok(v) => v,
                    Err(e) => return error(400, "invalid_folder", e),
                };
                state.data.lock().unwrap().unclassified_folder = folder;
            }
            return match persist(&state) {
                Ok(()) => ok(json!({"ok":true})),
                Err(e) => error(500, "settings_write_failed", e),
            };
        }
        return error(405, "method_not_allowed", "设置接口支持 GET/PUT");
    }
    if path == "/api/v1/directories" {
        let relative = url
            .query_pairs()
            .find(|(k, _)| k == "path")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let dir = match safe_library_path(&state, &relative) {
            Ok(v) => v,
            Err(e) => return error(400, "invalid_directory", e),
        };
        if method == Method::POST {
            let name = input["name"].as_str().unwrap_or("").trim();
            if name.is_empty() || name == "." || name == ".." || clean(name) != name {
                return error(400, "invalid_directory", "请输入有效的单层文件夹名称");
            }
            match fs::create_dir(dir.join(name)) {
                Ok(()) => return ok(json!({"ok":true})),
                Err(e) => return error(400, "directory_create_failed", e.to_string()),
            }
        }
        if method != Method::GET {
            return error(405, "method_not_allowed", "目录接口支持 GET/POST");
        }
        let mut entries = vec![];
        match fs::read_dir(&dir) {
            Ok(items) => {
                for entry in items.flatten() {
                    if entry.path().is_dir()
                        && fs::canonicalize(entry.path())
                            .is_ok_and(|p| p.starts_with(&state.library_root))
                    {
                        let rel = entry
                            .path()
                            .strip_prefix(&state.library_root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        entries
                            .push(json!({"name":entry.file_name().to_string_lossy(),"path":rel}));
                    }
                }
            }
            Err(e) => return error(403, "directory_access_denied", e.to_string()),
        }
        entries.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
        return ok(
            json!({"path":relative,"absolutePath":dir,"root":state.library_root,"directories":entries}),
        );
    }
    if path == "/api/v1/shares/parse" && method == Method::POST {
        return match parse_share_link(
            input["url"].as_str().unwrap_or("").into(),
            input["password"].as_str().unwrap_or("").into(),
        ) {
            Ok(value) => ok(serde_json::to_value(value).unwrap()),
            Err(e) => error(422, "share_parse_failed", e),
        };
    }
    if path == "/api/v1/tasks" {
        if method == Method::GET {
            return ok(serde_json::to_value(&*state.jobs.lock().unwrap()).unwrap());
        }
        if method == Method::POST {
            return match start_task(&state, &input) {
                Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
                Err(e) => error(400, "task_rejected", e),
            };
        }
    }
    if let Some(tail) = path.strip_prefix("/api/v1/tasks/") {
        let mut segments = tail.split('/');
        let id = segments.next().unwrap_or("");
        let operation = segments.next();
        let task = state
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .cloned();
        let Some(task) = task else {
            return error(404, "task_not_found", "任务不存在");
        };
        if method == Method::GET && operation.is_none() {
            return ok(serde_json::to_value(task).unwrap());
        }
        if method == Method::GET && operation == Some("results") {
            return ok(json!(task.results));
        }
        if method == Method::POST && operation == Some("cancel") {
            if matches!(task.status.as_str(), "queued" | "running") {
                let _ = update_task(&state, id, |t| {
                    t.status = "cancelled".into();
                    t.message = "任务已取消".into();
                });
            }
            return ok(json!({"ok":true}));
        }
        if method == Method::POST && operation == Some("retry") {
            if matches!(task.status.as_str(), "queued" | "running") {
                return error(409, "task_running", "任务正在运行");
            }
            let params =
                secret(&format!("task-{id}")).and_then(|v| serde_json::from_str::<Value>(&v).ok());
            let Some(mut params) = params else {
                return error(400, "retry_unavailable", "任务输入不可用，请重新提交分享");
            };
            params["outputRoot"] = json!(task.output_root);
            params["minSizeMb"] = json!(task.min_size_mb);
            params["strmSettings"] = json!(task.strm_settings);
            return match start_task(&state, &params) {
                Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
                Err(e) => error(400, "task_rejected", e),
            };
        }
    }
    error(404, "not_found", "接口不存在或不支持此请求方法")
}
async fn handle(State(state): State<Arc<ServerState>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 12 * 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return error(413, "request_too_large", "请求超过 12MB"),
    };
    let head = parts.method == Method::HEAD;
    let mut response = match tokio::task::spawn_blocking(move || {
        dispatch(
            state,
            parts.method,
            parts.uri.to_string(),
            parts.headers,
            body,
        )
    })
    .await
    {
        Ok(response) => response,
        Err(_) => error(500, "internal_error", "服务端请求处理异常"),
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    if head {
        *response.body_mut() = axum::body::Body::empty();
    }
    response
}
fn load_admin(dir: &Path) -> Result<Admin, String> {
    let path = dir.join("admin.json");
    if path.exists() {
        return serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string());
    }
    let username = std::env::var("MOVIETTD_ADMIN_USER").unwrap_or_else(|_| "admin".into());
    let password = std::env::var("MOVIETTD_ADMIN_PASSWORD")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().simple().to_string());
    if password.len() < 12 {
        return Err("管理员初始密码至少 12 个字符".into());
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "密码哈希失败")?
        .to_string();
    let admin = Admin {
        username: username.clone(),
        password_hash,
    };
    atomic_json(&path, &admin)?;
    private_file(&path)?;
    if std::env::var("MOVIETTD_ADMIN_PASSWORD").is_err() {
        let bootstrap = dir.join("initial-admin.json");
        atomic_json(
            &bootstrap,
            &json!({"username":username,"password":password}),
        )?;
        private_file(&bootstrap)?;
        println!("Initial admin credentials: {}", bootstrap.display());
    }
    Ok(admin)
}
fn load_state(dir: PathBuf, library_root: PathBuf) -> Result<Arc<ServerState>, String> {
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&library_root).map_err(|e| e.to_string())?;
    let library_root = fs::canonicalize(library_root).map_err(|e| e.to_string())?;
    if is_filesystem_root(&library_root) {
        return Err("媒体库目录不能设置为系统根目录".into());
    }
    let identity = dir.join("server-id");
    let server_id = if identity.exists() {
        fs::read_to_string(&identity).map_err(|e| e.to_string())?
    } else {
        let id = uuid::Uuid::new_v4().simple().to_string();
        fs::write(&identity, &id).map_err(|e| e.to_string())?;
        id
    };
    let admin = load_admin(&dir)?;
    let settings_path = dir.join("settings.json");
    let mut data: PersistedState = if settings_path.exists() {
        serde_json::from_slice(&fs::read(settings_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("配置文件损坏：{e}"))?
    } else {
        PersistedState {
            tmdb: Some(defaults()),
            strm: StrmSettings::default(),
            connections: vec![],
            proxy_base_url: std::env::var("MOVIETTD_PUBLIC_URL")
                .unwrap_or_else(|_| DEFAULT_PROXY_URL.into()),
            proxy_shares: vec![],
            proxy_files: vec![],
            unclassified_folder: default_unclassified_folder(),
        }
    };
    for connection in &mut data.connections {
        connection.connected = secret(&format!("drive-{}", connection.provider)).is_some();
    }
    let jobs_path = dir.join("tasks.json");
    let mut jobs: Vec<Task> = if jobs_path.exists() {
        serde_json::from_slice(&fs::read(jobs_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("任务记录损坏：{e}"))?
    } else {
        vec![]
    };
    for task in &mut jobs {
        if matches!(task.status.as_str(), "queued" | "running") {
            task.status = "interrupted".into();
            task.message = "服务已重启，请手动重试任务".into();
        }
    }
    let roots_path = dir.join("storage-roots.json");
    let mut storage_roots: Vec<PathBuf> = if roots_path.exists() {
        serde_json::from_slice(&fs::read(&roots_path).map_err(|e| e.to_string())?)
            .unwrap_or_default()
    } else {
        vec![library_root.clone()]
    };
    storage_roots
        .retain(|root| fs::canonicalize(root).is_ok_and(|p| p.is_dir() && !is_filesystem_root(&p)));
    if !storage_roots.contains(&library_root) {
        storage_roots.insert(0, library_root.clone());
    }
    atomic_json(&roots_path, &storage_roots)?;
    let state = Arc::new(ServerState {
        server_id,
        data: Mutex::new(data),
        dir: dir.clone(),
        library_root,
        storage_roots: Mutex::new(storage_roots),
        admin: Mutex::new(admin),
        sessions: Mutex::new(HashMap::new()),
        task_submit: Mutex::new(()),
        active_job: Mutex::new(None),
        attempts: Mutex::new(vec![]),
        jobs: Mutex::new(jobs),
        job_write: Mutex::new(()),
        config_write: Mutex::new(()),
        automation: Mutex::new(load_automation(&dir)?),
        telegram: Mutex::new(None),
    });
    persist(&state)?;
    persist_jobs(&state)?;
    Ok(state)
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize blocking reqwest and filesystem state outside Tokio worker contexts.
    let dir = PathBuf::from(std::env::var("MOVIETTD_DATA_DIR").unwrap_or_else(|_| "data".into()));
    let library = PathBuf::from(
        std::env::var("MOVIETTD_LIBRARY_ROOT")
            .unwrap_or_else(|_| dir.join("media").to_string_lossy().into()),
    );
    let state = tokio::task::spawn_blocking(move || {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let dir = fs::canonicalize(dir).map_err(|e| e.to_string())?;
        protect_data_directory(&dir)?;
        VAULT
            .set(Mutex::new(Vault::open(&dir)?))
            .map_err(|_| "凭证库重复初始化")?;
        load_state(dir, library)
    })
    .await??;
    let address = std::env::var("MOVIETTD_BIND").unwrap_or_else(|_| "127.0.0.1:18766".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!(
        "MovieTTD independent API ready: http://{address}; library={}",
        state.library_root.display()
    );
    start_automation_worker(&state);
    axum::serve(listener, Router::new().fallback(handle).with_state(state.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    stop_telegram(&state);
    Ok(())
}
include!("core_tests.rs");

include!("api_tests.rs");

include!("automation_tests.rs");
