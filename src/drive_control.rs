// All import and playback upstream calls share one gate; playback URLs are memory-only.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AutomationLimits {
    request_interval_secs: u64,
    task_gap_secs: u64,
    merge_secs: u64,
    daily_limit: usize,
    history_batch_size: usize,
    history_interval_secs: u64,
    playback_cache_secs: u64,
}
impl Default for AutomationLimits {
    fn default() -> Self {
        Self {
            request_interval_secs: 3,
            task_gap_secs: 45,
            merge_secs: 60,
            daily_limit: 20,
            history_batch_size: 30,
            history_interval_secs: 5,
            playback_cache_secs: 60,
        }
    }
}
struct DriveGate {
    limits: AutomationLimits,
    next: Option<Instant>,
    paused_until: i64,
    reason: String,
}
static DRIVE_PAUSE_PATH: OnceLock<PathBuf> = OnceLock::new();
static DRIVE_GATE: OnceLock<Mutex<DriveGate>> = OnceLock::new();
fn drive_gate() -> &'static Mutex<DriveGate> {
    DRIVE_GATE.get_or_init(|| {
        Mutex::new(DriveGate {
            limits: AutomationLimits::default(),
            next: None,
            paused_until: 0,
            reason: String::new(),
        })
    })
}
fn drive_wait() -> Result<(), String> {
    let mut gate = drive_gate().lock().map_err(|_| "网盘请求锁失败")?;
    if gate.paused_until > now() {
        return Err(format!(
            "网盘请求已暂停：{}；稍后或人工确认后恢复",
            gate.reason
        ));
    }
    let interval = if cfg!(test) {
        0
    } else {
        std::env::var("MOVIETTD_TEST_DRIVE_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(gate.limits.request_interval_secs * 1000)
    };
    if let Some(next) = gate.next {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
    }
    gate.next = Some(Instant::now() + Duration::from_millis(interval));
    Ok(())
}
fn persist_drive_pause(gate: &DriveGate) -> Result<(), String> {
    if let Some(path) = DRIVE_PAUSE_PATH.get() {
        atomic_json(
            path,
            &json!({"until":gate.paused_until,"reason":gate.reason}),
        )?;
    }
    Ok(())
}
fn restore_drive_pause(path: &Path) -> (i64, String) {
    if !path.exists() {
        return (0, String::new());
    }
    match fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        Some(value) if value["until"].is_i64() && value["reason"].is_string() => (
            value["until"].as_i64().unwrap(),
            value["reason"].as_str().unwrap().into(),
        ),
        _ => (now() + 86400, "限流记录读取失败，请检查后人工恢复".into()),
    }
}
fn drive_pause(seconds: u64, reason: &str) {
    if let Ok(mut gate) = drive_gate().lock() {
        gate.paused_until = now().saturating_add(seconds.max(1).min(i64::MAX as u64) as i64);
        gate.reason = reason.into();
        if persist_drive_pause(&gate).is_err() {
            eprintln!("Failed to persist upstream request pause");
        }
    }
}
fn check_drive_response(response: &reqwest::blocking::Response) -> Result<(), String> {
    let status = response.status().as_u16();
    if status == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.parse().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(v)
                        .ok()
                        .map(|dt| (dt.timestamp() - now()).max(1) as u64)
                })
            })
            .unwrap_or(900);
        drive_pause(seconds, "上游限流");
        return Err("网盘请求过于频繁，已暂停自动处理".into());
    }
    if status == 401 {
        drive_pause(86400, "网盘登录失效，需要重新登录");
        return Err("网盘登录失效，已暂停自动处理".into());
    }
    Ok(())
}
fn check_drive_body(value: &Value) -> Result<(), String> {
    if value["code"].as_i64() == Some(0) {
        return Ok(());
    }
    let message = value["message"].as_str().unwrap_or("网盘请求失败");
    if [
        "频繁",
        "风控",
        "验证",
        "异常访问",
        "登录失效",
        "登录过期",
        "token expired",
    ]
    .iter()
    .any(|word| message.to_lowercase().contains(word))
    {
        drive_pause(86400, "网盘返回验证、访问限制或登录失效，请人工检查");
    }
    Err(message.into())
}
struct CachedPlayback {
    url: String,
    expires: Instant,
}
static PLAYBACK: OnceLock<Mutex<HashMap<String, Arc<Mutex<Option<CachedPlayback>>>>>> =
    OnceLock::new();
fn cached_playback_link(key: &str, file: &RemoteFile, token: &str) -> Result<String, String> {
    let cache_key = format!("{}\n{}\n{}\n{}", key, file.id, file.etag, token_hash(token));
    let map = PLAYBACK.get_or_init(|| Mutex::new(HashMap::new()));
    let slot = {
        let mut m = map.lock().map_err(|_| "播放缓存锁失败")?;
        if m.len() > 2048 {
            m.retain(|_, v| {
                v.try_lock().map_or(true, |s| {
                    s.as_ref().is_some_and(|e| e.expires > Instant::now())
                })
            });
        }
        m.entry(cache_key)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    };
    let mut entry = slot.lock().map_err(|_| "播放请求锁失败")?;
    if let Some(value) = entry.as_ref() {
        if value.expires > Instant::now() {
            return Ok(value.url.clone());
        }
    }
    let url = direct_link(key, file, token)?;
    let ttl = drive_gate()
        .lock()
        .map_err(|_| "限速配置锁失败")?
        .limits
        .playback_cache_secs;
    *entry = Some(CachedPlayback {
        url: url.clone(),
        expires: Instant::now() + Duration::from_secs(ttl),
    });
    Ok(url)
}
