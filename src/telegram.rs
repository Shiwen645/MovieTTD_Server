// The worker has no public port. Only the authenticated Rust API can send commands.
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Sender};
static TG_START_LOCK: Mutex<()> = Mutex::new(());
static TG_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct TelegramBridge {
    child: Child,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<String, Sender<Value>>>>,
}
impl Drop for TelegramBridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn stop_telegram(state: &ServerState) {
    TG_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut slot) = state.telegram.lock() {
        *slot = None;
    }
}
fn telegram_send(state: &ServerState, mut input: Value) -> Result<Value, String> {
    if !["init", "status", "logout"].contains(&input["op"].as_str().unwrap_or("")) {
        let d = state.automation.lock().map_err(|_| "Telegram 状态锁失败")?;
        if d.tg_retry_at > now() {
            return Err(format!("Telegram 要求等待至 {} 后重试", d.tg_retry_at));
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    input["id"] = json!(id);
    let (tx, rx) = mpsc::channel();
    let pending = {
        let mut slot = state.telegram.lock().map_err(|_| "Telegram 进程锁失败")?;
        let bridge = slot
            .as_mut()
            .ok_or("Telegram 尚未启动，请先保存配置并连接")?;
        if bridge
            .child
            .try_wait()
            .map_err(|_| "无法检查 Telegram 进程")?
            .is_some()
        {
            return Err("Telegram 工作进程已退出，请重新连接".into());
        }
        bridge.pending.lock().unwrap().insert(id.clone(), tx);
        let line = format!(
            "{}\n",
            serde_json::to_string(&input).map_err(|_| "请求编码失败")?
        );
        if bridge
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| bridge.stdin.flush())
            .is_err()
        {
            bridge.pending.lock().unwrap().remove(&id);
            return Err("Telegram 进程通信失败".into());
        }
        bridge.pending.clone()
    };
    let result = rx.recv_timeout(Duration::from_secs(if input["op"] == "init" {
        45
    } else {
        25
    }));
    pending.lock().unwrap().remove(&id);
    let response = result.map_err(|_| "Telegram 请求超时，请检查服务器网络或代理")?;
    if response["ok"].as_bool() != Some(true) {
        let message = response["error"].as_str().unwrap_or("Telegram 操作失败");
        if let Some(seconds) = response["retryAfter"].as_i64().filter(|v| *v > 0) {
            let mut data = state.automation.lock().unwrap();
            data.tg_retry_at = now() + seconds;
            data.tg_error = message.into();
            save_automation(state, &data)?;
        }
        return Err(message.into());
    }
    Ok(response["data"].clone())
}
fn start_telegram(state: &Arc<ServerState>) -> Result<Value, String> {
    let _start = TG_START_LOCK.lock().map_err(|_| "Telegram 启动锁失败")?;
    {
        let mut d = state.automation.lock().unwrap();
        if d.tg_retry_at > now() {
            return Err("Telegram 仍在等待期，请稍后连接".into());
        }
        d.tg_authorized = false;
        save_automation(state, &d)?;
    }
    stop_telegram(state);
    let epoch = TG_GENERATION.load(std::sync::atomic::Ordering::SeqCst);
    let config: Value = serde_json::from_str(
        &secret("telegram-config").ok_or("请先配置 Telegram API_ID 和 API_HASH")?,
    )
    .map_err(|_| "TG 配置损坏")?;
    let executable = std::env::var("MOVIETTD_PYTHON").unwrap_or_else(|_| {
        let suffix = if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        };
        for base in ["telegram/.venv", "server/telegram/.venv"] {
            let candidate = Path::new(base).join(suffix);
            if candidate.is_file() {
                return candidate.to_string_lossy().into();
            }
        }
        if cfg!(windows) { "python" } else { "python3" }.into()
    });
    let mut command = Command::new(executable);
    command
        .args(["-u", "-c", include_str!("../telegram/worker.py")])
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(
        |_| "无法启动 Python；请配置 MOVIETTD_PYTHON 并安装 server/telegram/requirements.txt",
    )?;
    let stdin = child.stdin.take().ok_or("Telegram 输入通道失败")?;
    let stdout = child.stdout.take().ok_or("Telegram 输出通道失败")?;
    let pending: Arc<Mutex<HashMap<String, Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
    let reply = pending.clone();
    let weak = Arc::downgrade(state);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(state) = weak.upgrade() else {
                break;
            };
            if TG_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != epoch {
                break;
            }
            if value["event"] == "session" {
                if let Some(session) = value["session"].as_str() {
                    if let Err(error) = store_secret("telegram-session", session) {
                        let mut d = state.automation.lock().unwrap();
                        d.tg_error = error;
                    }
                }
            } else if value["event"] == "message" {
                if let Err(e) = receive_telegram_message(&state, &value["data"], None) {
                    let mut data = state.automation.lock().unwrap();
                    data.tg_error = e;
                    let channel = value["data"]["channelId"].as_str().unwrap_or("");
                    let from = value["data"]["messageId"]
                        .as_i64()
                        .unwrap_or(1)
                        .saturating_sub(1)
                        .max(0);
                    if let Some(m) = data
                        .monitors
                        .iter_mut()
                        .find(|m| m.enabled && m.channel_id == channel)
                    {
                        m.catchup_cursor =
                            Some(m.catchup_cursor.unwrap_or(m.last_message_id).min(from));
                        m.next_check_at = now() + 300;
                    }
                    let _ = save_automation(&state, &data);
                }
            } else if value["event"] == "fatal" {
                let message = value["message"].as_str().unwrap_or("Telegram 进程启动失败");
                let mut data = state.automation.lock().unwrap();
                data.tg_error = message.into();
                let _ = save_automation(&state, &data);
                for (_, sender) in reply.lock().unwrap().drain() {
                    let _ = sender.send(json!({"ok":false,"error":message}));
                }
            } else {
                if let Some(session) = value["data"]
                    .get("session")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                {
                    if store_secret("telegram-session", session).is_err() {
                        value = json!({"id":value["id"],"ok":false,"error":"TG 会话加密保存失败"});
                    }
                }
                if let Some(object) = value["data"].as_object_mut() {
                    object.remove("session");
                }
                if let Some(sender) = reply
                    .lock()
                    .unwrap()
                    .remove(value["id"].as_str().unwrap_or(""))
                {
                    let _ = sender.send(value);
                }
            }
        }
        if TG_GENERATION.load(std::sync::atomic::Ordering::SeqCst) == epoch {
            if let Some(state) = weak.upgrade() {
                let mut d = state.automation.lock().unwrap();
                d.tg_authorized = false;
                let _ = save_automation(&state, &d);
            }
        }
        for (_, sender) in reply.lock().unwrap().drain() {
            let _ = sender.send(json!({"ok":false,"error":"Telegram 工作进程已退出"}));
        }
    });
    *state.telegram.lock().unwrap() = Some(TelegramBridge {
        child,
        stdin,
        pending,
    });
    let watched: Vec<String> = state
        .automation
        .lock()
        .unwrap()
        .monitors
        .iter()
        .filter(|m| m.enabled)
        .map(|m| m.channel_id.clone())
        .collect();
    let result = telegram_send(
        state,
        json!({"op":"init","watched":watched,"apiId":config["apiId"],"apiHash":config["apiHash"],"proxy":config["proxy"],"session":secret("telegram-session").unwrap_or_default()}),
    )?;
    {
        let mut data = state.automation.lock().unwrap();
        data.tg_authorized = result["authorized"].as_bool().unwrap_or(false);
        data.tg_error = String::new();
        for monitor in &mut data.monitors {
            if monitor.enabled {
                monitor.catchup_cursor = Some(
                    monitor
                        .catchup_cursor
                        .unwrap_or(monitor.last_message_id)
                        .min(monitor.last_message_id),
                );
            }
        }
        save_automation(state, &data)?;
    }
    if result["authorized"] == true {
        sync_telegram_watches(state)?;
    }
    Ok(result)
}
fn sync_telegram_watches(state: &ServerState) -> Result<(), String> {
    let channels: Vec<Value> = {
        let data = state.automation.lock().unwrap();
        data.monitors
            .iter()
            .filter(|m| m.enabled)
            .map(|m| json!(m.reference))
            .collect()
    };
    telegram_send(state, json!({"op":"watch","channels":channels}))?;
    Ok(())
}
fn telegram_api(state: &Arc<ServerState>, method: &Method, path: &str, input: &Value) -> Response {
    let result = (|| -> Result<Value, String> {
        match (method.as_str(), path) {
            ("GET", "/api/v1/telegram") => {
                let config: Value = secret("telegram-config")
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(json!({}));
                let mut running = false;
                if let Some(bridge) = state.telegram.lock().unwrap().as_mut() {
                    running = bridge
                        .child
                        .try_wait()
                        .map_err(|_| "无法检查进程")?
                        .is_none();
                }
                let data = state.automation.lock().unwrap();
                Ok(
                    json!({"apiId":config["apiId"],"configured":config["apiHash"].as_str().is_some_and(|v|!v.is_empty()),"proxyConfigured":config["proxy"].as_str().is_some_and(|v|!v.is_empty()),"authorized":data.tg_authorized,"running":running,"error":data.tg_error,"retryAt":data.tg_retry_at}),
                )
            }
            ("PUT", "/api/v1/telegram") => {
                let old: Value = secret("telegram-config")
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(json!({}));
                let id = input["apiId"]
                    .as_i64()
                    .filter(|v| *v > 0 && *v <= i32::MAX as i64)
                    .ok_or("API_ID 必须是有效正整数")?;
                let hash = input["apiHash"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .or(old["apiHash"].as_str())
                    .ok_or("请输入 API_HASH")?;
                if hash.len() != 32 || !hash.chars().all(|v| v.is_ascii_hexdigit()) {
                    return Err("API_HASH 应为 32 位十六进制字符".into());
                }
                let proxy = if input["clearProxy"] == true {
                    ""
                } else {
                    input["proxy"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .or(old["proxy"].as_str())
                        .unwrap_or("")
                };
                if !proxy.is_empty() {
                    let parsed = reqwest::Url::parse(proxy).map_err(|_| "TG 代理地址无效")?;
                    if !["http", "socks5", "socks4"].contains(&parsed.scheme())
                        || parsed.host_str().is_none()
                        || parsed.port().is_none()
                    {
                        return Err("TG 代理需要 http/socks5/socks4 地址和显式端口".into());
                    }
                }
                if old["apiId"] != json!(id) || old["apiHash"] != json!(hash) {
                    store_secret("telegram-session", "")?;
                }
                store_secret(
                    "telegram-config",
                    &json!({"apiId":id,"apiHash":hash,"proxy":proxy}).to_string(),
                )?;
                start_telegram(state)
            }
            ("POST", "/api/v1/telegram/connect") => start_telegram(state),
            ("POST", "/api/v1/telegram/login") => {
                {
                    let data = state.automation.lock().unwrap();
                    if data.tg_retry_at > now() {
                        return Err(format!("Telegram 要求等待至 {} 后重试", data.tg_retry_at));
                    }
                }
                let action = input["action"].as_str().unwrap_or("");
                if !["send_code", "sign_in", "password"].contains(&action) {
                    return Err("登录步骤无效".into());
                }
                let phone = input["phone"].as_str().unwrap_or("");
                let code = input["code"].as_str().unwrap_or("");
                let password = input["password"].as_str().unwrap_or("");
                if action == "send_code"
                    && (!phone.starts_with('+')
                        || phone.len() < 7
                        || phone.len() > 20
                        || !phone[1..].chars().all(|c| c.is_ascii_digit()))
                {
                    return Err("手机号应包含国际区号，例如 +86…".into());
                }
                if action == "sign_in"
                    && (code.len() < 4
                        || code.len() > 10
                        || !code.chars().all(|c| c.is_ascii_digit()))
                {
                    return Err("请输入有效验证码".into());
                }
                if action == "password" && (password.is_empty() || password.len() > 256) {
                    return Err("请输入两步验证密码".into());
                }
                {
                    let mut data = state.automation.lock().unwrap();
                    if action == "send_code" {
                        if data.last_code_at + 60 > now() {
                            return Err("请至少等待 60 秒再请求验证码".into());
                        }
                        data.last_code_at = now();
                        save_automation(state, &data)?;
                    }
                }
                let result = telegram_send(
                    state,
                    json!({"op":action,"phone":phone,"code":code,"password":password}),
                )?;
                if result["stage"] == "authorized" {
                    {
                        let mut data = state.automation.lock().unwrap();
                        data.tg_authorized = true;
                        data.tg_error.clear();
                        save_automation(state, &data)?;
                    }
                    sync_telegram_watches(state)?;
                }
                Ok(result)
            }
            ("POST", "/api/v1/telegram/logout") => {
                if state.telegram.lock().unwrap().is_some() {
                    telegram_send(state, json!({"op":"logout"}))?;
                }
                stop_telegram(state);
                store_secret("telegram-session", "")?;
                let mut data = state.automation.lock().unwrap();
                data.tg_authorized = false;
                save_automation(state, &data)?;
                Ok(json!({"ok":true}))
            }
            _ => Err("Telegram 接口或请求方法不存在".into()),
        }
    })();
    match result {
        Ok(value) => ok(value),
        Err(e) => error(400, "telegram_error", e),
    }
}
