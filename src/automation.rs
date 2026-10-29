// Shared by manual Telegram history, live events, and scheduled share updates.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AutomationData {
    limits: AutomationLimits,
    paused: bool,
    monitors: Vec<ChannelMonitor>,
    history: Vec<HistoryImport>,
    queue: Vec<AutoImport>,
    tracking: Vec<ShareTracking>,
    seen_messages: HashMap<String, String>,
    tg_authorized: bool,
    tg_error: String,
    tg_retry_at: i64,
    tg_next_request_at: i64,
    last_code_at: i64,
    last_task_at: i64,
    day: i64,
    daily_started: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelMonitor {
    id: String,
    reference: String,
    channel_id: String,
    title: String,
    output_root: String,
    include: String,
    exclude: String,
    enabled: bool,
    last_message_id: i64,
    catchup_cursor: Option<i64>,
    next_check_at: i64,
    last_error: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryImport {
    id: String,
    reference: String,
    channel_id: String,
    title: String,
    output_root: String,
    include: String,
    exclude: String,
    before_id: i64,
    min_id: i64,
    max_messages: usize,
    scanned: usize,
    status: String,
    next_run_at: i64,
    error: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoImport {
    id: String,
    dedup_key: String,
    share_key: String,
    title: String,
    source: String,
    output_root: String,
    status: String,
    task_id: Option<String>,
    attempts: usize,
    next_run_at: i64,
    created_at: i64,
    updated_at: i64,
    error: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareTracking {
    id: String,
    share_key: String,
    output_root: String,
    interval_hours: u64,
    expires_at: i64,
    next_run_at: i64,
    enabled: bool,
}
fn load_automation(dir: &Path) -> Result<AutomationData, String> {
    let file = dir.join("automation.json");
    let mut data: AutomationData = if file.exists() {
        serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
            .map_err(|_| "自动入库记录损坏")?
    } else {
        AutomationData::default()
    };
    data.tg_authorized = false;
    for m in &mut data.monitors {
        if m.enabled {
            m.catchup_cursor = Some(
                m.catchup_cursor
                    .unwrap_or(m.last_message_id)
                    .min(m.last_message_id),
            );
        }
    }
    Ok(data)
}
fn save_automation(state: &ServerState, data: &AutomationData) -> Result<(), String> {
    atomic_json(&state.dir.join("automation.json"), data)
}
fn auto_output(state: &ServerState, input: &Value) -> Result<String, String> {
    let path = input["outputRoot"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| state.library_root.to_string_lossy().into());
    Ok(storage_path(state, &path)?.to_string_lossy().into())
}
fn keyword_match(text: &str, include: &str, exclude: &str) -> bool {
    let text = text.to_lowercase();
    let words = |value: &str| {
        value
            .split(['|', ',', '，', '\n'])
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let include = words(include);
    let exclude = words(exclude);
    (include.is_empty() || include.iter().any(|v| text.contains(v)))
        && !exclude.iter().any(|v| text.contains(v))
}
fn telegram_share_links(text: &str) -> Vec<(String, String)> {
    let re = Regex::new(r#"https?://[^\s<>\"，。]+"#).unwrap();
    let mut result = Vec::new();
    let links: Vec<_> = re.find_iter(text).collect();
    for (i, m) in links.iter().enumerate() {
        let raw = m
            .as_str()
            .trim_end_matches([')', ']', '）', ',', ';', '；']);
        let Ok(url) = reqwest::Url::parse(raw) else {
            continue;
        };
        let host = url.host_str().unwrap_or("").trim_start_matches("www.");
        if ![
            "123pan.com",
            "123pan.cn",
            "123865.com",
            "123684.com",
            "123912.com",
            "123952.com",
            "123pan.net",
        ]
        .contains(&host)
        {
            continue;
        }
        let Ok(key) = share_key(raw) else {
            continue;
        };
        let end = links.get(i + 1).map_or(text.len(), |v| v.start());
        let tail = &text[m.end()..end];
        let password=url.query_pairs().find(|(k,_)|["pwd","password"].contains(&k.as_ref())).map(|(_,v)|v.to_string()).unwrap_or_else(||{
            Regex::new(r"(?i)(?:提取码|提取碼|访问码|密码|密碼|pwd|password)\s*[:：=]?\s*([a-z0-9]{4,8})(?:[^a-z0-9]|$)").unwrap().captures(tail).map(|c|c[1].into()).unwrap_or_default()
        });
        if !result.contains(&(key.clone(), password.clone())) {
            result.push((key, password));
        }
    }
    result
}
fn enqueue_share(
    _state: &ServerState,
    data: &mut AutomationData,
    key: &str,
    password: &str,
    root: &str,
    title: &str,
    source: &str,
    force: bool,
) -> Result<bool, String> {
    let dedup = token_hash(&format!("{key}\n{password}\n{root}"));
    if data.queue.iter().any(|q| {
        q.dedup_key == dedup
            && (!force
                || matches!(
                    q.status.as_str(),
                    "pending" | "starting" | "running" | "paused"
                ))
    }) {
        return Ok(false);
    }
    if data
        .queue
        .iter()
        .filter(|q| {
            matches!(
                q.status.as_str(),
                "pending" | "starting" | "running" | "paused"
            )
        })
        .count()
        >= 5000
    {
        return Err("自动入库队列已达 5000 条，请先处理积压任务".into());
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    store_secret(
        &format!("auto-{id}"),
        &json!({"url":format!("https://www.123pan.com/s/{key}"),"password":password}).to_string(),
    )?;
    data.queue.push(AutoImport {
        id,
        dedup_key: dedup,
        share_key: key.into(),
        title: title.chars().take(160).collect(),
        source: source.into(),
        output_root: root.into(),
        status: "pending".into(),
        task_id: None,
        attempts: 0,
        next_run_at: now() + data.limits.merge_secs as i64,
        created_at: now(),
        updated_at: now(),
        error: String::new(),
    });
    Ok(true)
}
fn receive_telegram_message(
    state: &ServerState,
    message: &Value,
    history: Option<&HistoryImport>,
) -> Result<(), String> {
    let channel = message["channelId"].as_str().ok_or("频道消息缺少频道 ID")?;
    let id = message["messageId"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or("消息 ID 无效")?;
    let text = message["text"].as_str().unwrap_or("");
    let mut data = state.automation.lock().map_err(|_| "自动入库锁失败")?;
    let (root, title, include, exclude, source) = if let Some(h) = history {
        (
            h.output_root.clone(),
            h.title.clone(),
            h.include.clone(),
            h.exclude.clone(),
            format!("history:{}", h.id),
        )
    } else {
        let Some(m) = data
            .monitors
            .iter_mut()
            .find(|m| m.enabled && m.channel_id == channel)
        else {
            return Ok(());
        };
        (
            m.output_root.clone(),
            m.title.clone(),
            m.include.clone(),
            m.exclude.clone(),
            format!("monitor:{}", m.id),
        )
    };
    let message_key = format!("{channel}:{id}:{}", token_hash(&root));
    let signature = token_hash(text);
    let edited = data.seen_messages.contains_key(&message_key);
    if data.seen_messages.get(&message_key) == Some(&signature) {
        if history.is_none() {
            if let Some(m) = data
                .monitors
                .iter_mut()
                .find(|m| m.enabled && m.channel_id == channel)
            {
                m.last_message_id = m.last_message_id.max(id);
            }
            save_automation(state, &data)?;
        }
        return Ok(());
    }
    if data.seen_messages.len() >= 500000 {
        return Err("消息去重索引达到上限，请归档历史导入记录".into());
    }
    if keyword_match(text, &include, &exclude) {
        for (key, password) in telegram_share_links(text) {
            enqueue_share(
                state,
                &mut data,
                &key,
                &password,
                &root,
                &format!("{title} · 消息 {id}"),
                &source,
                edited,
            )?;
        }
    }
    if history.is_none() {
        if let Some(m) = data
            .monitors
            .iter_mut()
            .find(|m| m.enabled && m.channel_id == channel)
        {
            m.last_message_id = m.last_message_id.max(id);
        }
    }
    data.seen_messages.insert(message_key, signature);
    save_automation(state, &data)
}
fn automation_api(
    state: &Arc<ServerState>,
    method: &Method,
    path: &str,
    input: &Value,
) -> Response {
    let result = (|| -> Result<Value, String> {
        if path == "/api/v1/automation" && method == Method::GET {
            let data = state.automation.lock().unwrap();
            let gate = drive_gate().lock().unwrap();
            return Ok(
                json!({"limits":data.limits,"paused":data.paused,"monitors":data.monitors,"history":data.history,"queue":data.queue.iter().rev().take(500).collect::<Vec<_>>(),"queueTotal":data.queue.len(),"pendingCount":data.queue.iter().filter(|q|q.status=="pending").count(),"tracking":data.tracking,"dailyStarted":if data.day==now()/86400{data.daily_started}else{0},"tgAuthorized":data.tg_authorized,"tgError":data.tg_error,"tgRetryAt":data.tg_retry_at,"drivePausedUntil":gate.paused_until,"drivePauseReason":gate.reason}),
            );
        }
        if path == "/api/v1/automation/limits" && method == Method::PUT {
            let limits: AutomationLimits =
                serde_json::from_value(input.clone()).map_err(|_| "限速配置格式无效")?;
            if !(1..=60).contains(&limits.request_interval_secs)
                || !(10..=3600).contains(&limits.task_gap_secs)
                || !(10..=3600).contains(&limits.merge_secs)
                || !(1..=1000).contains(&limits.daily_limit)
                || !(1..=100).contains(&limits.history_batch_size)
                || !(3..=3600).contains(&limits.history_interval_secs)
                || !(0..=300).contains(&limits.playback_cache_secs)
            {
                return Err("请求间隔 1–60 秒、任务间隔/合并等待 10–3600 秒、每日任务 1–1000、历史批次 1–100 条、历史间隔 3–3600 秒、播放缓存 0–300 秒".into());
            }
            let mut data = state.automation.lock().unwrap();
            data.limits = limits.clone();
            save_automation(state, &data)?;
            drive_gate().lock().unwrap().limits = limits;
            return Ok(json!({"ok":true}));
        }
        if path == "/api/v1/automation/control" && method == Method::POST {
            let mut data = state.automation.lock().unwrap();
            let action = input["action"].as_str().unwrap_or("");
            match action {
                "pause" => data.paused = true,
                "resume" => data.paused = false,
                "resume-drive" => {
                    let mut gate = drive_gate().lock().unwrap();
                    gate.paused_until = 0;
                    gate.reason.clear();
                    persist_drive_pause(&gate)?;
                    for q in &mut data.queue {
                        if q.status == "paused" {
                            q.status = "pending".into();
                            q.next_run_at = now() + 60;
                        }
                    }
                }
                _ => return Err("操作无效".into()),
            }
            save_automation(state, &data)?;
            return Ok(json!({"ok":true}));
        }
        if path == "/api/v1/automation/monitors" && method == Method::POST {
            let reference = input["channel"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() < 256)
                .ok_or("请输入频道用户名、链接或 ID")?;
            let root = auto_output(state, input)?;
            let resolved = telegram_send(state, json!({"op":"resolve","channel":reference}))?;
            let mut data = state.automation.lock().unwrap();
            if data
                .monitors
                .iter()
                .any(|m| m.channel_id == resolved["channelId"].as_str().unwrap_or(""))
            {
                return Err("该频道已在监控列表中".into());
            }
            let cursor = resolved["latestId"].as_i64().unwrap_or(0);
            data.monitors.push(ChannelMonitor {
                id: uuid::Uuid::new_v4().simple().to_string(),
                reference: reference.into(),
                channel_id: resolved["channelId"].as_str().ok_or("频道解析失败")?.into(),
                title: resolved["title"].as_str().unwrap_or(reference).into(),
                output_root: root,
                include: input["include"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                exclude: input["exclude"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                enabled: true,
                last_message_id: cursor,
                catchup_cursor: Some(cursor),
                next_check_at: now(),
                last_error: String::new(),
            });
            save_automation(state, &data)?;
            drop(data);
            sync_telegram_watches(state)?;
            return Ok(json!({"ok":true}));
        }
        if path == "/api/v1/automation/history" && method == Method::POST {
            let reference = input["channel"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() < 256)
                .ok_or("请输入频道")?;
            let root = auto_output(state, input)?;
            let resolved = telegram_send(state, json!({"op":"resolve","channel":reference}))?;
            let count = input["maxMessages"].as_u64().unwrap_or(200) as usize;
            if !(1..=100000).contains(&count) {
                return Err("历史消息数量应为 1–100000".into());
            }
            let latest = resolved["latestId"].as_i64().unwrap_or(0);
            let lower = input["fromMessageId"].as_i64().unwrap_or(1);
            let upper = input["toMessageId"]
                .as_i64()
                .filter(|v| *v > 0)
                .unwrap_or(latest)
                .min(latest);
            if lower < 1 || upper < lower {
                return Err("消息 ID 范围无效，或频道暂无消息".into());
            }
            let mut data = state.automation.lock().unwrap();
            data.history.push(HistoryImport {
                id: uuid::Uuid::new_v4().simple().to_string(),
                reference: reference.into(),
                channel_id: resolved["channelId"].as_str().ok_or("频道解析失败")?.into(),
                title: resolved["title"].as_str().unwrap_or(reference).into(),
                output_root: root,
                include: input["include"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                exclude: input["exclude"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                before_id: upper + 1,
                min_id: lower - 1,
                max_messages: count,
                scanned: 0,
                status: "pending".into(),
                next_run_at: now(),
                error: String::new(),
            });
            save_automation(state, &data)?;
            return Ok(json!({"ok":true}));
        }
        if path == "/api/v1/automation/tracking" && method == Method::POST {
            let raw = input["url"].as_str().ok_or("请输入分享链接")?;
            let key = share_key(raw)?;
            let password = extract_password(raw, input["password"].as_str().unwrap_or(""));
            let root = auto_output(state, input)?;
            let hours = input["intervalHours"].as_u64().unwrap_or(12);
            let days = input["days"].as_u64().unwrap_or(30);
            if !(6..=720).contains(&hours) || !(1..=365).contains(&days) {
                return Err("追更间隔 6–720 小时，持续 1–365 天".into());
            }
            let id = uuid::Uuid::new_v4().simple().to_string();
            store_secret(&format!("tracking-{id}"), &password)?;
            let mut data = state.automation.lock().unwrap();
            if data
                .tracking
                .iter()
                .any(|t| t.share_key == key && t.output_root == root)
            {
                return Err("该分享已在追更列表中".into());
            }
            data.tracking.push(ShareTracking {
                id,
                share_key: key,
                output_root: root,
                interval_hours: hours,
                expires_at: now() + days as i64 * 86400,
                next_run_at: now(),
                enabled: true,
            });
            save_automation(state, &data)?;
            return Ok(json!({"ok":true}));
        }
        if method == Method::POST {
            let kind = path
                .strip_prefix("/api/v1/automation/")
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("");
            let id = input["id"].as_str().ok_or("缺少记录 ID")?;
            let action = input["action"].as_str().unwrap_or("");
            let mut data = state.automation.lock().unwrap();
            match kind {
                "monitors" => {
                    let m = data
                        .monitors
                        .iter_mut()
                        .find(|m| m.id == id)
                        .ok_or("监控不存在")?;
                    match action {
                        "pause" => m.enabled = false,
                        "resume" => {
                            m.enabled = true;
                            m.catchup_cursor = Some(
                                m.catchup_cursor
                                    .unwrap_or(m.last_message_id)
                                    .min(m.last_message_id),
                            );
                            m.next_check_at = now();
                        }
                        "remove" => data.monitors.retain(|m| m.id != id),
                        _ => return Err("监控操作无效".into()),
                    };
                    save_automation(state, &data)?;
                    drop(data);
                    if state.automation.lock().unwrap().tg_authorized {
                        sync_telegram_watches(state)?;
                    }
                    return Ok(json!({"ok":true}));
                }
                "history" => {
                    let h = data
                        .history
                        .iter_mut()
                        .find(|h| h.id == id)
                        .ok_or("历史任务不存在")?;
                    match action {
                        "pause" => h.status = "paused".into(),
                        "resume" => {
                            h.status = "pending".into();
                            h.error.clear();
                            h.next_run_at = now();
                        }
                        "cancel" => h.status = "cancelled".into(),
                        _ => return Err("历史任务操作无效".into()),
                    };
                }
                "queue" => {
                    let q = data
                        .queue
                        .iter_mut()
                        .find(|q| q.id == id)
                        .ok_or("队列任务不存在")?;
                    if matches!(q.status.as_str(), "running" | "starting") {
                        return Err("请在服务端任务页面停止运行中的任务".into());
                    }
                    match action {
                        "retry" => {
                            q.status = "pending".into();
                            q.attempts = 0;
                            q.next_run_at = now() + 60;
                            q.error.clear();
                            q.task_id = None;
                        }
                        "cancel" => q.status = "cancelled".into(),
                        _ => return Err("队列操作无效".into()),
                    };
                }
                "tracking" => {
                    let t = data
                        .tracking
                        .iter_mut()
                        .find(|t| t.id == id)
                        .ok_or("追更不存在")?;
                    match action {
                        "pause" => t.enabled = false,
                        "resume" => {
                            t.enabled = true;
                            t.next_run_at = now();
                        }
                        "remove" => data.tracking.retain(|t| t.id != id),
                        _ => return Err("追更操作无效".into()),
                    };
                }
                _ => return Err("自动入库接口不存在".into()),
            }
            save_automation(state, &data)?;
            return Ok(json!({"ok":true}));
        }
        Err("自动入库接口或请求方法不存在".into())
    })();
    match result {
        Ok(v) => ok(v),
        Err(e) => error(400, "automation_error", e),
    }
}
fn tick_telegram(state: &Arc<ServerState>) -> Result<(), String> {
    let (authorized, paused, retry, limits) = {
        let d = state.automation.lock().unwrap();
        (d.tg_authorized, d.paused, d.tg_retry_at, d.limits.clone())
    };
    if !authorized || paused || retry > now() {
        return Ok(());
    }
    {
        let mut d = state.automation.lock().unwrap();
        if d.tg_next_request_at > now() {
            return Ok(());
        }
        d.tg_next_request_at = now() + limits.history_interval_secs as i64;
    }
    let monitored = {
        let d = state.automation.lock().unwrap();
        d.monitors
            .iter()
            .find(|m| m.enabled && m.catchup_cursor.is_some() && m.next_check_at <= now())
            .cloned()
    };
    if let Some(m) = monitored {
        let after = m.catchup_cursor.unwrap_or(m.last_message_id);
        let response = telegram_send(
            state,
            json!({"op":"history","channel":m.reference,"minId":after,"forward":true,"limit":limits.history_batch_size}),
        );
        match response {
            Ok(result) => {
                let messages = result["messages"].as_array().ok_or("TG 消息响应格式错误")?;
                let scope = HistoryImport {
                    id: format!("catchup-{}", m.id),
                    reference: m.reference.clone(),
                    channel_id: m.channel_id.clone(),
                    title: m.title.clone(),
                    output_root: m.output_root.clone(),
                    include: m.include.clone(),
                    exclude: m.exclude.clone(),
                    before_id: 0,
                    min_id: 0,
                    max_messages: 0,
                    scanned: 0,
                    status: String::new(),
                    next_run_at: 0,
                    error: String::new(),
                };
                for message in messages {
                    receive_telegram_message(state, message, Some(&scope))?;
                }
                let cursor = messages
                    .iter()
                    .filter_map(|v| v["messageId"].as_i64())
                    .max()
                    .unwrap_or(after);
                let mut d = state.automation.lock().unwrap();
                if let Some(item) = d.monitors.iter_mut().find(|item| item.id == m.id) {
                    item.last_message_id = item.last_message_id.max(cursor);
                    item.catchup_cursor = if result["done"] == true {
                        None
                    } else {
                        Some(cursor)
                    };
                    item.next_check_at = now() + limits.history_interval_secs as i64;
                    item.last_error.clear();
                }
                save_automation(state, &d)?;
            }
            Err(error) => {
                let mut d = state.automation.lock().unwrap();
                if let Some(item) = d.monitors.iter_mut().find(|item| item.id == m.id) {
                    item.last_error = error;
                    item.next_check_at = now() + 300;
                }
                save_automation(state, &d)?;
            }
        }
        return Ok(());
    }
    let history = {
        let d = state.automation.lock().unwrap();
        d.history
            .iter()
            .find(|h| h.status == "pending" && h.next_run_at <= now())
            .cloned()
    };
    if let Some(h) = history {
        let limit = limits
            .history_batch_size
            .min(h.max_messages.saturating_sub(h.scanned));
        if limit == 0 {
            let mut d = state.automation.lock().unwrap();
            if let Some(h) = d.history.iter_mut().find(|v| v.id == h.id) {
                h.status = "completed".into();
            }
            save_automation(state, &d)?;
            return Ok(());
        }
        let response = telegram_send(
            state,
            json!({"op":"history","channel":h.reference,"minId":h.min_id,"maxId":h.before_id,"limit":limit}),
        );
        match response {
            Ok(result) => {
                let messages = result["messages"].as_array().ok_or("TG 消息响应格式错误")?;
                for message in messages {
                    receive_telegram_message(state, message, Some(&h))?;
                }
                let before = messages
                    .iter()
                    .filter_map(|v| v["messageId"].as_i64())
                    .min()
                    .unwrap_or(h.before_id);
                let mut d = state.automation.lock().unwrap();
                if let Some(item) = d.history.iter_mut().find(|item| item.id == h.id) {
                    item.scanned += messages.len();
                    item.before_id = before;
                    item.next_run_at = now() + limits.history_interval_secs as i64;
                    if result["done"] == true
                        || item.scanned >= item.max_messages
                        || before <= item.min_id + 1
                    {
                        item.status = "completed".into();
                    }
                }
                save_automation(state, &d)?;
            }
            Err(error) => {
                let mut d = state.automation.lock().unwrap();
                if let Some(item) = d.history.iter_mut().find(|item| item.id == h.id) {
                    item.error = error;
                    item.status = "paused".into();
                }
                save_automation(state, &d)?;
            }
        }
    }
    Ok(())
}
fn tick_import_queue(state: &Arc<ServerState>) -> Result<(), String> {
    let tasks = state.jobs.lock().unwrap().clone();
    let mut data = state.automation.lock().unwrap();
    let mut changed = false;
    for q in &mut data.queue {
        if q.status != "running" {
            continue;
        }
        let task = q
            .task_id
            .as_ref()
            .and_then(|id| tasks.iter().find(|t| &t.id == id));
        let Some(task) = task else {
            q.status = "paused".into();
            q.error = "关联任务缺失，请检查后重试".into();
            changed = true;
            continue;
        };
        if matches!(task.status.as_str(), "queued" | "running") {
            continue;
        }
        changed = true;
        q.updated_at = now();
        if task.status == "completed" {
            q.status = "completed".into();
            q.error.clear();
            continue;
        }
        let reason = task.error.clone().unwrap_or_else(|| {
            task.results
                .iter()
                .filter(|r| r.status == "failed")
                .map(|r| r.message.clone())
                .take(3)
                .collect::<Vec<_>>()
                .join("；")
        });
        let transient = [
            "超时",
            "无法连接",
            "网络",
            "HTTP 500",
            "HTTP 502",
            "HTTP 503",
            "HTTP 504",
            "下载中断",
        ]
        .iter()
        .any(|s| reason.contains(s));
        if transient && q.attempts < 4 {
            q.status = "pending".into();
            q.next_run_at = now() + [60, 300, 900][q.attempts.saturating_sub(1).min(2)];
            q.task_id = None;
        } else {
            q.status =
                if drive_gate().lock().unwrap().paused_until > now() || reason.contains("登录") {
                    "paused"
                } else if task.status == "cancelled" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
        }
        q.error = if reason.is_empty() {
            task.message.clone()
        } else {
            reason
        };
    }
    if changed {
        save_automation(state, &data)?;
    }
    if data.paused || drive_gate().lock().unwrap().paused_until > now() {
        return Ok(());
    }
    if data.day != now() / 86400 {
        data.day = now() / 86400;
        data.daily_started = 0;
        save_automation(state, &data)?;
    }
    if data.daily_started >= data.limits.daily_limit
        || now() < data.last_task_at + data.limits.task_gap_secs as i64
    {
        return Ok(());
    }
    if state.active_job.lock().unwrap().is_some() {
        return Ok(());
    }
    let due = data
        .tracking
        .iter()
        .find(|t| t.enabled && t.expires_at > now() && t.next_run_at <= now())
        .cloned();
    if let Some(t) = due {
        let password = secret(&format!("tracking-{}", t.id)).unwrap_or_default();
        enqueue_share(
            state,
            &mut data,
            &t.share_key,
            &password,
            &t.output_root,
            &format!("追更 · {}", t.share_key),
            &format!("tracking:{}", t.id),
            true,
        )?;
        if let Some(t) = data.tracking.iter_mut().find(|item| item.id == t.id) {
            t.next_run_at = now() + t.interval_hours as i64 * 3600;
        }
        save_automation(state, &data)?;
    }
    let Some(item) = data
        .queue
        .iter()
        .find(|q| q.status == "pending" && q.next_run_at <= now())
        .cloned()
    else {
        return Ok(());
    };
    // A durable reservation precedes task creation. Restart recovery does not silently replay uncertain work.
    let reservation = data.queue.iter_mut().find(|q| q.id == item.id).unwrap();
    reservation.status = "starting".into();
    reservation.updated_at = now();
    save_automation(state, &data)?;
    drop(data);
    let result = (|| -> Result<Value, String> {
        let mut params: Value =
            serde_json::from_str(&secret(&format!("auto-{}", item.id)).ok_or("队列分享凭证缺失")?)
                .map_err(|_| "队列输入损坏")?;
        params["outputRoot"] = json!(item.output_root);
        start_task(state, &params)
    })();
    let mut data = state.automation.lock().unwrap();
    match result {
        Ok(task) => {
            let q = data.queue.iter_mut().find(|q| q.id == item.id).unwrap();
            q.status = "running".into();
            q.task_id = task["id"].as_str().map(str::to_string);
            q.attempts += 1;
            q.updated_at = now();
            data.daily_started += 1;
            data.last_task_at = now();
        }
        Err(e) => {
            let q = data.queue.iter_mut().find(|q| q.id == item.id).unwrap();
            q.status = if e.contains("登录") {
                "paused"
            } else if e.contains("任务") {
                "pending"
            } else {
                "failed"
            }
            .into();
            q.next_run_at = now() + 60;
            q.error = e;
        }
    }
    save_automation(state, &data)
}
fn start_automation_worker(state: &Arc<ServerState>) {
    let limits = state.automation.lock().unwrap().limits.clone();
    let pause_path = state.dir.join("drive-pause.json");
    let _ = DRIVE_PAUSE_PATH.set(pause_path.clone());
    {
        let mut gate = drive_gate().lock().unwrap();
        gate.limits = limits;
        let (until, reason) = restore_drive_pause(&pause_path);
        gate.paused_until = until;
        gate.reason = reason;
    }
    // Recover a crash between recording a queue reservation and recording its task ID.
    {
        let mut d = state.automation.lock().unwrap();
        for q in &mut d.queue {
            if q.status == "starting" {
                q.status = "paused".into();
                q.error = "服务重启时任务提交状态不确定，请检查服务端任务后重试".into();
            }
        }
        let _ = save_automation(state, &d);
    }
    let weak = Arc::downgrade(state);
    thread::spawn(move || {
        let mut connect_at = 0;
        loop {
            thread::sleep(Duration::from_secs(2));
            let Some(state) = weak.upgrade() else {
                break;
            };
            let running = state
                .telegram
                .lock()
                .unwrap()
                .as_mut()
                .is_some_and(|b| b.child.try_wait().is_ok_and(|exit| exit.is_none()));
            if !running
                && now() >= connect_at
                && secret("telegram-session").is_some()
                && state.automation.lock().unwrap().tg_retry_at <= now()
            {
                match start_telegram(&state) {
                    Ok(_) => connect_at = now() + 300,
                    Err(e) => {
                        let mut d = state.automation.lock().unwrap();
                        d.tg_error = e;
                        let _ = save_automation(&state, &d);
                        connect_at = now() + 300;
                    }
                }
            }
            if let Err(e) = tick_import_queue(&state) {
                let mut d = state.automation.lock().unwrap();
                d.tg_error = e;
                let _ = save_automation(&state, &d);
            }
            if let Err(e) = tick_telegram(&state) {
                let mut d = state.automation.lock().unwrap();
                d.tg_error = e;
                let _ = save_automation(&state, &d);
            }
        }
    });
}
