#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmdbSettings {
    pub api_key: String,
    pub base_url: String,
    pub image_base_url: String,
    pub api_key_configured: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub provider: String,
    pub label: String,
    pub connected: bool,
    #[serde(default)]
    pub quota: Option<Value>,
}
#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    tmdb: Option<TmdbSettings>,
    #[serde(default)]
    strm: StrmSettings,
    connections: Vec<Connection>,
    #[serde(default)]
    proxy_base_url: String,
    #[serde(default)]
    proxy_shares: Vec<ProxyShare>,
    #[serde(default)]
    proxy_files: Vec<ProxyFile>,
    #[serde(default = "default_unclassified_folder")]
    unclassified_folder: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyShare {
    id: String,
    share_key: String,
    password_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyFile {
    id: String,
    share_id: String,
    file_id: String,
    relative_path: String,
    file: RemoteFile,
}

#[derive(Clone, Serialize, Deserialize)]
struct RemoteFile {
    id: String,
    name: String,
    is_dir: bool,
    size: u64,
    s3: String,
    etag: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StrmSettings {
    video_extensions: String,
    min_size_mb: u64,
    metadata_extensions: String,
    duplicate_policy: String,
}
impl Default for StrmSettings {
    fn default() -> Self {
        Self {
            video_extensions: "mp4,mkv,avi,mov,m2ts,ts,wmv,flv,rmvb,webm,m4v,rm,mpg,mpeg,iso"
                .into(),
            min_size_mb: 100,
            metadata_extensions: "ass,srt,ssa,sub,vtt".into(),
            duplicate_policy: "skip".into(),
        }
    }
}
fn extension_list(value: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for ext in value
        .split([',', '，'])
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if ext.len() > 16
            || !ext.chars().all(|c| c.is_ascii_alphanumeric())
            || ext.eq_ignore_ascii_case("strm")
        {
            return Err("扩展名须为不带点号的字母或数字，不能包含 strm".into());
        }
        let ext = ext.to_ascii_lowercase();
        if !result.contains(&ext) {
            result.push(ext);
        }
    }
    if result.len() > 100 {
        return Err("扩展名数量过多".into());
    }
    Ok(result)
}
fn validated_strm_settings(input: &Value) -> Result<StrmSettings, String> {
    let mut value: StrmSettings =
        serde_json::from_value(input.clone()).map_err(|_| "STRM 设置格式错误")?;
    if !["skip", "larger", "replace", "keep"].contains(&value.duplicate_policy.as_str()) {
        return Err("重复资源规则无效".into());
    }
    let video = extension_list(&value.video_extensions)?;
    let metadata = extension_list(&value.metadata_extensions)?;
    if video.is_empty() {
        return Err("至少保留一个视频扩展名".into());
    }
    if video.iter().any(|v| metadata.contains(v)) {
        return Err("视频与元数据扩展名不能重复".into());
    }
    if value.min_size_mb > 10240 {
        return Err("小视频阈值不能超过 10240 MB".into());
    }
    value.video_extensions = video.join(",");
    value.metadata_extensions = metadata.join(",");
    Ok(value)
}
fn has_extension(name: &str, list: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|ext| list.split(',').any(|v| v.eq_ignore_ascii_case(ext)))
}
fn defaults() -> TmdbSettings {
    TmdbSettings {
        api_key: String::new(),
        base_url: DEFAULT_API.into(),
        image_base_url: DEFAULT_IMAGE.into(),
        api_key_configured: false,
    }
}
fn default_unclassified_folder() -> String {
    "未分类".into()
}
fn validate_unclassified_folder(folder: &str) -> Result<String, String> {
    let folder = folder.trim();
    if folder.is_empty()
        || clean(folder) != folder
        || folder.len() > 180
        || ["动漫", "电影", "剧集", "综艺"].contains(&folder)
        || folder.chars().any(|c| c.is_control())
        || Regex::new(r"(?i)^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)")
            .unwrap()
            .is_match(folder)
    {
        return Err("未分类目录必须是有效的单层文件夹名称，且不能与四个媒体分类重名".into());
    }
    Ok(folder.to_string())
}
fn validate_123_token(token: &str) -> Result<(), String> {
    let c = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let device_id = pan123_device_id()?;
    let r = pan123_validation_request(&c, token, &device_id)
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                "123 云盘验证超时，请检查网络".to_string()
            } else {
                "无法连接 123 云盘验证服务".to_string()
            }
        })?;
    if !r.status().is_success() {
        return Err(format!("123 云盘验证服务返回 HTTP {}", r.status().as_u16()));
    }
    let v: Value = r
        .json()
        .map_err(|_| "123 云盘返回了无法识别的验证结果".to_string())?;
    validate_account_response("pan123", &v)
}

fn pan123_validation_request(
    client: &Client,
    token: &str,
    device_id: &str,
) -> reqwest::blocking::RequestBuilder {
    let (auth_key, auth_value) = sign("/b/api/user/info");
    client
        .get(provider_url(
            "MOVIETTD_123_USER_URL",
            "https://yun.123pan.cn/b/api/user/info",
        ))
        .header("Authorization", format!("Bearer {token}"))
        .header("platform", "web")
        .header("app-version", "3")
        .header("loginuuid", device_id)
        .header("auth-key", auth_key)
        .header("auth-value", auth_value)
        .header("Accept", "application/json, text/plain, */*")
        .header("User-Agent", UA)
}

fn validate_account_response(provider: &str, json: &Value) -> Result<(), String> {
    let valid = match provider {
        "pan123" => {
            json["code"].as_i64() == Some(0)
                && json["data"]["Nickname"]
                    .as_str()
                    .is_some_and(|name| !name.trim().is_empty())
        }
        "quark" | "uc" => {
            json["success"].as_bool() == Some(true)
                && json["data"]["nickname"]
                    .as_str()
                    .is_some_and(|name| !name.trim().is_empty())
        }
        _ => false,
    };
    if valid {
        return Ok(());
    }
    // Never echo server text: some gateways include request credentials in errors.
    let code = json["code"]
        .as_i64()
        .map(|value| format!("（code={value}）"))
        .unwrap_or_default();
    Err(format!(
        "网盘未接受此登录凭证{code}，请确认已完成登录后重试"
    ))
}

fn pan123_device_id() -> Result<String, String> {
    static DEVICE_ID: Mutex<Option<String>> = Mutex::new(None);
    let mut cached = DEVICE_ID.lock().map_err(|_| "设备标识锁失败".to_string())?;
    if let Some(id) = cached.as_ref() {
        return Ok(id.clone());
    }
    if let Some(id) = secret("pan123-device-id")
        .filter(|id| id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()))
    {
        *cached = Some(id.clone());
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    store_secret("pan123-device-id", &id)
        .map_err(|_| "无法安全保存 123 云盘设备标识".to_string())?;
    *cached = Some(id.clone());
    Ok(id)
}
fn cookie_is_plausible(cookie: &str) -> bool {
    let mut pus = false;
    let mut puus = false;
    for part in cookie.split(';') {
        if let Some((name, value)) = part.trim().split_once('=') {
            if value.trim().is_empty() {
                continue;
            }
            pus |= name == "__pus";
            puus |= name == "__puus";
        }
    }
    pus && puus
}
fn validate_cookie(provider: &str, cookie: &str) -> Result<(), String> {
    if !cookie_is_plausible(cookie) {
        return Err("登录已完成，但尚未取得完整网盘 Cookie，请稍后重试".into());
    }
    let (url, user_agent) = match provider {
        "quark" => (
            "https://pan.quark.cn/account/info",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) quark-cloud-drive/2.5.20 Chrome/100.0.4896.160 Electron/18.3.5.12-a038f7b798 Safari/537.36 Channel/pckk_other_ch",
        ),
        "uc" => ("https://drive.uc.cn/account/info", UA),
        _ => return Err("未知网盘平台".into()),
    };
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "无法初始化网盘验证服务".to_string())?;
    let response = client
        .get(url)
        .header("Cookie", cookie)
        .header("User-Agent", user_agent)
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                "网盘登录态验证超时，请检查网络".to_string()
            } else {
                "无法连接网盘登录态验证服务".to_string()
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "网盘验证服务返回 HTTP {}",
            response.status().as_u16()
        ));
    }
    let json: Value = response
        .json()
        .map_err(|_| "网盘返回了无法识别的验证结果".to_string())?;
    validate_account_response(provider, &json)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareResult {
    provider: String,
    share_key: String,
    title: String,
    password_required: bool,
}
fn share_key(text: &str) -> Result<String, String> {
    let url = Regex::new(r"https?://[^\s]+").unwrap();
    let raw = url
        .find(text)
        .map(|m| m.as_str().trim_end_matches(['。', '，', ',', ')']))
        .unwrap_or(text);
    let re = Regex::new(r"/s/([A-Za-z0-9]+-[A-Za-z0-9]+)").unwrap();
    if let Some(c) = re.captures(raw) {
        return Ok(c[1].into());
    }
    if raw.contains("123pan") || raw.contains("123865") {
        if let Some(seg) = raw.split('/').filter(|x| !x.is_empty()).last() {
            let val = seg.split('?').next().unwrap_or("");
            if !val.is_empty() && val != "s" {
                return Ok(val.into());
            }
        }
    }
    Err("无法从123云盘链接识别分享码".into())
}
fn extract_password(text: &str, password: &str) -> String {
    if !password.trim().is_empty() {
        return password.trim().into();
    }
    if let Some(raw)=Regex::new(r"https?://[^\s]+").unwrap().find(text){
        if let Ok(url)=reqwest::Url::parse(raw.as_str()){
            if let Some((_,value))=url.query_pairs().find(|(key,value)|["pwd","password"].contains(&key.as_ref())&&!value.is_empty()) {return value.into_owned();}
        }
    }
    Regex::new(r"(?i)(?:提取码|提取碼|访问码|密码|密碼|pwd|password)\s*[:：=]?\s*([A-Za-z0-9]{4,8})(?:[^a-z0-9]|$)")
        .unwrap().captures(text).map(|c|c[1].into()).unwrap_or_default()
}
// Prefer top-level share folders; direct-file shares use their filenames instead.
fn share_source_names(files: &[RemoteFile]) -> Vec<String> {
    let folders: Vec<String> = files
        .iter()
        .filter(|f| f.is_dir)
        .map(|f| f.name.clone())
        .collect();
    if folders.is_empty() {
        files.iter().map(|f| f.name.clone()).collect()
    } else {
        folders
    }
}
fn parse_share_link(url: String, password: String) -> Result<ShareResult, String> {
    let key = share_key(&url)?;
    let pwd = extract_password(&url, &password);
    let files = list_share(&key, &pwd, "0")?;
    Ok(ShareResult {
        provider: "123 云盘".into(),
        share_key: key,
        title: share_source_names(&files).join("，"),
        password_required: false,
    })
}
fn list_share(key: &str, password: &str, parent: &str) -> Result<Vec<RemoteFile>, String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut all = Vec::new();
    for page in 1..=200 {
        let mut q = vec![
            ("limit", "100".to_string()),
            ("next", "0".to_string()),
            ("orderBy", "file_name".into()),
            ("orderDirection", "asc".into()),
            ("shareKey", key.into()),
            ("ParentFileId", parent.into()),
            ("Page", page.to_string()),
        ];
        if !password.is_empty() {
            q.push(("SharePwd", password.into()))
        }
        drive_wait()?;
        let r = client
            .get(provider_url(
                "MOVIETTD_123_SHARE_URL",
                "https://yun.123pan.cn/b/api/share/get",
            ))
            .query(&q)
            .header("User-Agent", UA)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    "读取123分享目录超时".to_string()
                } else {
                    "无法连接123分享目录服务".to_string()
                }
            })?;
        check_drive_response(&r)?;
        match r.status().as_u16() {
            403 => return Err("123分享提取码错误".into()),
            404 => return Err("123分享或目录不存在".into()),
            410 => return Err("123分享已失效或取消".into()),
            status if !(200..300).contains(&status) => {
                return Err(format!("123分享服务返回 HTTP {status}"));
            }
            _ => {}
        }
        let v: Value = r.json().map_err(|e| e.to_string())?;
        check_drive_body(&v)?;
        if v["code"].as_i64() != Some(0) {
            return Err(v["message"]
                .as_str()
                .unwrap_or("读取分享目录失败，请检查提取码")
                .into());
        }
        let data = &v["data"];
        if data["Expired"].as_bool() == Some(true) {
            return Err("123分享已失效".into());
        }
        let items = data["InfoList"].as_array().cloned().unwrap_or_default();
        for it in &items {
            let id = it["FileId"]
                .as_i64()
                .map(|x| x.to_string())
                .or_else(|| it["FileId"].as_str().map(str::to_string))
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            all.push(RemoteFile {
                id,
                name: it["FileName"].as_str().unwrap_or("未命名文件").into(),
                is_dir: it["Type"].as_i64() == Some(1),
                size: it["Size"].as_u64().unwrap_or(0),
                s3: it["S3KeyFlag"].as_str().unwrap_or("").into(),
                etag: it["Etag"].as_str().unwrap_or("").into(),
            })
        }
        if items.is_empty() || data["Next"].as_str() == Some("-1") {
            break;
        }
    }
    Ok(all)
}
fn sign(path: &str) -> (String, String) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let minute = Utc
        .timestamp_opt((ts + 57600) as i64, 0)
        .single()
        .unwrap()
        .format("%Y%m%d%H%M")
        .to_string();
    let table = "adefghlmyijnopkqrstubcvwsz";
    let mapped: String = minute
        .chars()
        .map(|c| table.chars().nth(c.to_digit(10).unwrap() as usize).unwrap())
        .collect();
    let key = format!("{:x}", crc32fast::hash(mapped.as_bytes()));
    let nonce =
        u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap()) % 10_000_000;
    let raw = format!("{ts}|{nonce}|{path}|web|3|{key}");
    let value = format!("{ts}-{nonce}-{:x}", crc32fast::hash(raw.as_bytes()));
    (key, value)
}
fn direct_link(key: &str, file: &RemoteFile, token: &str) -> Result<String, String> {
    let path = "/b/api/share/download/info";
    let (ak, av) = sign(path);
    let body = json!({"ShareKey":key,"FileID":file.id,"S3KeyFlag":file.s3,"Size":file.size,"Etag":file.etag});
    let c = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    drive_wait()?;
    let r = c
        .post(format!(
            "{}{path}",
            provider_url("MOVIETTD_123_DIRECT_BASE", "https://www.123865.com")
        ))
        .header("platform", "android")
        .header("app-version", "39")
        .header("authorization", format!("Bearer {token}"))
        .header("loginuuid", pan123_device_id()?)
        .header("auth-key", ak)
        .header("auth-value", av)
        .header("User-Agent", UA)
        .json(&body)
        .send()
        .map_err(|e| format!("获取分享直链失败：{e}"))?;
    check_drive_response(&r)?;
    let v: Value = r.json().map_err(|e| e.to_string())?;
    check_drive_body(&v)?;
    if v["code"].as_i64() != Some(0) {
        return Err(v["message"].as_str().unwrap_or("获取直链失败").into());
    }
    let raw = v["data"]["DownloadURL"].as_str().unwrap_or("");
    let encoded = raw
        .split("params=")
        .nth(1)
        .map(|x| x.split('&').next().unwrap_or(x))
        .unwrap_or(raw);
    if let Ok(bytes) = URL_SAFE.decode(format!(
        "{encoded}{}",
        "=".repeat((4 - encoded.len() % 4) % 4)
    )) {
        if let Ok(decoded) = String::from_utf8(bytes) {
            if decoded.starts_with("http") {
                return Ok(decoded);
            }
        }
    }
    if raw.starts_with("http") {
        Ok(raw.into())
    } else {
        Err("123云盘没有返回有效直链".into())
    }
}

#[derive(Default)]
struct MediaName {
    title: String,
    year: String,
    kind: String,
    season: Option<u32>,
    episode: Option<u32>,
    source_parent: PathBuf,
}
fn parse_media(name: &str) -> MediaName {
    let stem = if is_video(name) {
        Path::new(name)
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or(name)
    } else {
        name
    };
    let normalized = stem.replace(['（', '【'], "[").replace(['）', '】'], "]");
    let mut working = normalized.clone();
    // Strip leading release groups, but retain bracketed media titles.
    let leading = Regex::new(r"^\s*\[([^\]]+)\]\s*").unwrap();
    while let Some(c) = leading.captures(&working) {
        let group = &c[1];
        let tail = &working[c[0].len()..];
        let known=group.contains("字幕") || group.contains("发布") || group.contains("压制") || Regex::new(r"(?i)^(?:ANi|LoliHouse|VCB-Studio|NC-Raws|B-Global|Ohys-Raws|Moozzi2|Lilith-Raws|桜都|樱都|喵萌|诸神|极影|动漫国|爱恋|悠哈|天使|北宇).*").unwrap().is_match(group);
        let ascii_group = group.is_ascii()
            && !group.contains(' ')
            && tail.starts_with('[')
            && !tail
                .trim_start_matches('[')
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
        if known || ascii_group {
            working = tail.into();
        } else {
            break;
        }
    }
    let ep = Regex::new(r"(?i)[ ._\-]*S(\d{1,3})[ ._\-]*E(\d{1,4})").unwrap();
    let cross = Regex::new(r"(?i)(?:^|[ ._\-])(\d{1,2})x(\d{1,4})(?:\b|[ ._\-])").unwrap();
    let simple = Regex::new(r"(?i)(?:\bEP?|第)\s*0*(\d{1,4})\s*(?:集|话|話)?").unwrap();
    let cn = Regex::new(r"第\s*([一二三四五六七八九十百零〇两]+)\s*[集话話]").unwrap();
    let anime = Regex::new(r"(?i)(?:\s-\s*|\[)(\d{1,3})(?:v\d)?(?:\]|\s|$)").unwrap();
    let mut cut = None;
    let (season, episode) = if let Some(c) = ep.captures(&working) {
        cut = Some(c.get(0).unwrap().start());
        (c[1].parse().ok(), c[2].parse().ok())
    } else if let Some(c) = cross.captures(&working) {
        cut = Some(c.get(0).unwrap().start());
        (c[1].parse().ok(), c[2].parse().ok())
    } else if let Some(c) = simple.captures(&working) {
        cut = Some(c.get(0).unwrap().start());
        (
            Some(season_from_directory_name(&working).unwrap_or(1)),
            c[1].parse().ok(),
        )
    } else if let Some(c) = cn.captures(&working) {
        cut = Some(c.get(0).unwrap().start());
        (
            Some(season_from_directory_name(&working).unwrap_or(1)),
            season_from_directory_name(&format!("第{}季", &c[1])),
        )
    } else if let Some(c) = anime.captures(&working) {
        cut = Some(c.get(0).unwrap().start());
        (None, c[1].parse().ok())
    } else {
        (None, None)
    };
    // Only standalone year tokens are dates; resolution tokens and title numbers remain intact.
    let yr = Regex::new(r"(?:^|[ ._\[\](){}\-])((?:19|20)\d{2})(?:[ ._\[\](){}\-]|$)").unwrap();
    let bracket_year = Regex::new(r"[\[(]((?:19|20)\d{2})[\])]").unwrap();
    let year_match = bracket_year
        .captures(&working)
        .and_then(|c| c.get(1))
        .or_else(|| yr.captures_iter(&working).last().and_then(|c| c.get(1)));
    let year = year_match
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let mut title = working[..cut.unwrap_or(working.len())].to_string();
    let year_ranges: Vec<_> = yr
        .captures_iter(&working)
        .filter_map(|c| c.get(1))
        .filter(|m| m.as_str() == year && m.end() <= title.len())
        .map(|m| m.range())
        .collect();
    for range in year_ranges.into_iter().rev() {
        title.replace_range(range, " ");
    }
    title=Regex::new(r"(?i)(?:\b(?:special[ ._\-]+edition|extended[ ._\-]+(?:edition|cut)|director.?s[ ._\-]+cut|hybrid|2160p|1080[pi]|720p|480p|4k|8k|sdr|hdr(?:10\+?)?|dv|dovi|dolby[ ._\-]?vision|h\.?26[45]|hevc|x26[45]|av1|web[ ._\-]?dl|webrip|blu[ ._\-]?ray|bdrip|remux|proper|repack|imax|uhd|bdremux|bd25|bd50|bd66|bd100|ddp|eac3|aac|ac3|dts(?:-hd)?|truehd|flac|atmos|\d+(?:\.\d+)?fps)\b|简繁|简体|繁体|中英字幕|中文字幕|国粤双语|国语配音|中字|内封字幕)").unwrap().split(&title).next().unwrap_or("").to_string();
    title = Regex::new(r"(?i)\s+\d+(?:[ .]\d+)?\s*(?:GB|GiB|MB|TB)\b.*$")
        .unwrap()
        .replace(&title, "")
        .to_string();
    title=Regex::new(r"(?i)(?:(?:^|[ ._\-]+)(?:season|s)\s*\d{1,3}|第\s*[0-9一二三四五六七八九十百零〇两]+\s*季).*").unwrap().replace(&title,"").to_string();
    title = Regex::new(
        r"(?i)(?:[ ._\-]+|[\[(])(?:全集|合集|全\d+[集季]|complete|collection)(?:[\])]|$)",
    )
    .unwrap()
    .replace_all(&title, " ")
    .to_string();
    title = Regex::new(r"[._\-\[\](){}]+|\s+")
        .unwrap()
        .replace_all(&title, " ")
        .trim()
        .trim_matches([':', ' ', '：'])
        .to_string();
    let season = season.or_else(|| season_from_directory_name(&normalized));
    MediaName {
        title,
        year,
        kind: if episode.is_some() || season.is_some() {
            "tv"
        } else {
            "movie"
        }
        .into(),
        season,
        episode,
        source_parent: PathBuf::new(),
    }
}
fn title_candidates(title: &str) -> Vec<String> {
    let mut variants = vec![title.trim().to_string()];
    let is_cjk =
        |c: char| ('\u{3400}'..='\u{9fff}').contains(&c) || ('\u{3040}'..='\u{30ff}').contains(&c);
    if let Some((index, _)) = title.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        if index > 0 && title[..index].chars().any(is_cjk) {
            variants.push(title[..index].trim().to_string());
            variants.push(title[index..].trim().to_string());
        } else if let Some((index, _)) = title.char_indices().find(|(_, c)| is_cjk(*c)) {
            if index > 0 {
                variants.push(title[..index].trim().to_string());
                variants.push(title[index..].trim().to_string());
            }
        }
    }
    for v in variants.clone() {
        // Remove explicit aliases and common title decorations, without removing sequel digits.
        let cleaned = Regex::new(r"(?i)\s*(?:又名|别名|別名|aka)\s*[:：]?.*$")
            .unwrap()
            .replace(&v, "")
            .trim()
            .to_string();
        if !variants.contains(&cleaned) {
            variants.push(cleaned);
        }
    }
    let mut unique = Vec::new();
    for v in variants {
        if !v.is_empty() && !unique.contains(&v) {
            unique.push(v);
        }
    }
    unique
}
fn normalized_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn title_similarity(query: &str, candidate: &str) -> f64 {
    let a = normalized_title(query);
    let b = normalized_title(candidate);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    if digits(&a) != digits(&b) {
        return 0.0;
    }
    let (short, long) = if a.chars().count() < b.chars().count() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.contains(short) {
        return short.chars().count() as f64 / long.chars().count() as f64;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=bc.len()).collect();
    for (i, x) in ac.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in bc.iter().enumerate() {
            let previous = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(previous + 1)
                .min(diagonal + usize::from(x != y));
            diagonal = previous;
        }
    }
    1.0 - row[bc.len()] as f64 / ac.len().max(bc.len()) as f64
}
fn tmdb_match(name: &MediaName, s: &TmdbSettings) -> Option<Value> {
    if s.api_key.is_empty() || name.title.is_empty() {
        return None;
    }
    let c = Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .ok()?;
    let endpoint = if name.kind == "tv" { "tv" } else { "movie" };
    let mut queries = title_candidates(&name.title);
    let primary_count = queries.len();
    for q in queries.clone() {
        let words: Vec<_> = q.split_whitespace().collect();
        if words.len() > 3 && q.is_ascii() {
            for trim in 1..=3.min(words.len() - 3) {
                let shorter = words[..words.len() - trim].join(" ");
                if !queries.contains(&shorter) {
                    queries.push(shorter);
                }
            }
        }
    }
    let mut best: Option<(f64, f64, Value)> = None;
    let mut aliases: HashMap<u64, Vec<String>> = HashMap::new();
    let mut alias_requests = 0;
    for with_year in [true, false] {
        if with_year && name.year.is_empty() {
            continue;
        }
        for (query_index, query) in queries.iter().enumerate() {
            if query_index >= primary_count && (with_year || best.is_some()) {
                continue;
            }
            let mut req = c
                .get(format!(
                    "{}/search/{endpoint}",
                    s.base_url.trim_end_matches('/')
                ))
                .query(&[
                    ("api_key", s.api_key.as_str()),
                    ("language", "zh-CN"),
                    ("query", query.as_str()),
                ]);
            if with_year {
                req = req.query(&[(
                    if endpoint == "tv" {
                        "first_air_date_year"
                    } else {
                        "year"
                    },
                    name.year.as_str(),
                )]);
            }
            let response = match req
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json::<Value>())
            {
                Ok(v) => v,
                Err(_) => continue,
            };
            for cand in response["results"].as_array().into_iter().flatten() {
                if cand["id"].as_u64().is_none() {
                    continue;
                }
                let mut similarity = queries[..primary_count]
                    .iter()
                    .flat_map(|q| {
                        ["name", "title", "original_name", "original_title"]
                            .map(move |key| title_similarity(q, cand[key].as_str().unwrap_or("")))
                    })
                    .fold(0.0, f64::max);
                if similarity < 0.68 && !with_year && query_index < primary_count {
                    let id = cand["id"].as_u64().unwrap();
                    if !aliases.contains_key(&id) && alias_requests < 6 {
                        alias_requests += 1;
                        let titles = c
                            .get(format!(
                                "{}/{endpoint}/{id}/alternative_titles",
                                s.base_url.trim_end_matches('/')
                            ))
                            .query(&[("api_key", s.api_key.as_str())])
                            .send()
                            .and_then(|r| r.error_for_status())
                            .and_then(|r| r.json::<Value>())
                            .ok()
                            .map(|v| {
                                v[if endpoint == "tv" {
                                    "results"
                                } else {
                                    "titles"
                                }]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|t| t["title"].as_str().map(str::to_string))
                                .collect()
                            })
                            .unwrap_or_default();
                        aliases.insert(id, titles);
                    }
                    if let Some(titles) = aliases.get(&id) {
                        similarity = similarity.max(
                            queries[..primary_count]
                                .iter()
                                .flat_map(|q| titles.iter().map(move |t| title_similarity(q, t)))
                                .fold(0.0, f64::max),
                        );
                    }
                }
                if similarity < 0.68 {
                    continue;
                }
                let year = cand["first_air_date"]
                    .as_str()
                    .or(cand["release_date"].as_str())
                    .unwrap_or("")
                    .get(..4)
                    .unwrap_or("");
                let score = similarity
                    + if year == name.year && !year.is_empty() {
                        0.12
                    } else {
                        0.0
                    };
                let popularity = cand["popularity"].as_f64().unwrap_or(0.0);
                if best
                    .as_ref()
                    .is_none_or(|(old, p, _)| score > *old || (score == *old && popularity > *p))
                {
                    best = Some((score, popularity, cand.clone()));
                }
            }
            if best.as_ref().is_some_and(|(score, _, _)| *score >= 1.10) {
                break;
            }
        }
        if best.as_ref().is_some_and(|(score, _, _)| *score >= 1.10) {
            break;
        }
    }
    let candidate = best?.2;
    Some(tmdb_metadata(&candidate, endpoint, name, s))
}
fn tmdb_detail(id: u64, endpoint: &str, settings: &TmdbSettings) -> Option<Value> {
    if settings.api_key.is_empty() {
        return None;
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .ok()?;
    let detail: Value = client
        .get(format!(
            "{}/{endpoint}/{id}",
            settings.base_url.trim_end_matches('/')
        ))
        .query(&[
            ("api_key", settings.api_key.as_str()),
            ("language", "zh-CN"),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .ok()?
        .json()
        .ok()?;
    (detail["id"].as_u64() == Some(id)).then_some(detail)
}
fn tmdb_metadata(
    candidate: &Value,
    endpoint: &str,
    name: &MediaName,
    settings: &TmdbSettings,
) -> Value {
    let id = candidate["id"].as_u64().unwrap_or_default();
    let search_genres = metadata_genre_ids(candidate).filter(|ids| !ids.is_empty());
    let details = search_genres
        .is_none()
        .then(|| tmdb_detail(id, endpoint, settings))
        .flatten();
    let source = details.as_ref().unwrap_or(candidate);
    let title = source["name"]
        .as_str()
        .filter(|title| !title.is_empty())
        .or_else(|| source["title"].as_str().filter(|title| !title.is_empty()))
        .or_else(|| candidate["name"].as_str().filter(|title| !title.is_empty()))
        .or_else(|| {
            candidate["title"]
                .as_str()
                .filter(|title| !title.is_empty())
        })
        .unwrap_or(&name.title);
    let date = source["first_air_date"]
        .as_str()
        .or_else(|| source["release_date"].as_str())
        .or_else(|| candidate["first_air_date"].as_str())
        .or_else(|| candidate["release_date"].as_str())
        .unwrap_or("");
    let genres = metadata_genre_ids(source)
        .filter(|ids| !ids.is_empty())
        .or(search_genres)
        .or_else(|| metadata_genre_ids(source));
    json!({
        "id": id,
        "title": title,
        "year": date.get(..4).unwrap_or(""),
        "poster": source["poster_path"].as_str().or(candidate["poster_path"].as_str()).unwrap_or(""),
        "overview": source["overview"].as_str().or(candidate["overview"].as_str()).unwrap_or(""),
        "mediaType": endpoint,
        "genreIds": genres.map(|ids| json!(ids)).unwrap_or(Value::Null)
    })
}
fn tmdb_id_from_path(path: &Path) -> Option<u64> {
    let pattern = Regex::new(r"(?i)tmdb(?:id)?[-_ ]*(\d+)").ok()?;
    path.components()
        .rev()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(|component| {
            pattern
                .captures(component)
                .and_then(|captures| captures.get(1))
                .and_then(|id| id.as_str().parse().ok())
        })
}
fn tmdb_match_for_import(
    name: &MediaName,
    relative_path: &Path,
    settings: &TmdbSettings,
) -> Option<Value> {
    let endpoint = if name.kind == "tv" { "tv" } else { "movie" };
    if let Some(id) = tmdb_id_from_path(relative_path) {
        if let Some(details) = tmdb_detail(id, endpoint, settings) {
            return Some(tmdb_metadata(&details, endpoint, name, settings));
        }
    }
    if let Some(found) = tmdb_match(name, settings) {
        return Some(found);
    }
    // If directory cleanup selected an unhelpful title, retain the actual release filename as fallback.
    let mut alternatives = Vec::new();
    if let Some(file) = relative_path.file_name().and_then(|v| v.to_str()) {
        let mut candidate = parse_media(file);
        candidate.kind = name.kind.clone();
        candidate.season = name.season;
        candidate.episode = name.episode;
        if !candidate.title.is_empty() && candidate.title != name.title {
            alternatives.push(candidate);
        }
    }
    if name.kind == "movie" {
        if let Some(folder) = relative_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
        {
            let candidate = parse_media(folder);
            if !candidate.title.is_empty()
                && candidate.title != name.title
                && (!candidate.year.is_empty() || tmdb_id_from_path(Path::new(folder)).is_some())
            {
                alternatives.push(candidate);
            }
        }
    }
    alternatives
        .into_iter()
        .take(2)
        .find_map(|candidate| tmdb_match(&candidate, settings))
}
fn episode_title(id: u64, season: u32, episode: u32, s: &TmdbSettings) -> String {
    if s.api_key.is_empty() {
        return String::new();
    }
    Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .ok()
        .and_then(|c| {
            c.get(format!(
                "{}/tv/{id}/season/{season}/episode/{episode}",
                s.base_url.trim_end_matches('/')
            ))
            .query(&[("api_key", s.api_key.as_str()), ("language", "zh-CN")])
            .send()
            .ok()?
            .json::<Value>()
            .ok()
        })
        .and_then(|v| v["name"].as_str().map(str::to_string))
        .unwrap_or_default()
}
fn clean(s: &str) -> String {
    Regex::new(r#"[\\/:*?"<>|\x00-\x1f]"#)
        .unwrap()
        .replace_all(s, " ")
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([' ', '.'])
        .to_string()
}

fn season_from_directory_name(name: &str) -> Option<u32> {
    let numeric_pattern =
        Regex::new(r"(?i)(?:^|[ ._-])(?:season|s)\s*0*(\d{1,3})|第\s*0*(\d{1,3})\s*季").ok()?;
    if let Some(season) = numeric_pattern
        .captures(name)
        .and_then(|captures| captures.get(1).or_else(|| captures.get(2)))
        .and_then(|value| value.as_str().parse().ok())
    {
        return Some(season);
    }
    let chinese_pattern = Regex::new(r"第([一二三四五六七八九十百零〇两]+)季").ok()?;
    let number = chinese_pattern.captures(name)?.get(1)?.as_str();
    let digit = |character| match character {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    };
    let mut total = 0u32;
    let mut pending = None;
    for character in number.chars() {
        if let Some(value) = digit(character) {
            pending = Some(pending.unwrap_or(0) * 10 + value);
        } else {
            let unit = match character {
                '十' => 10,
                '百' => 100,
                _ => return None,
            };
            total += pending.take().unwrap_or(1) * unit;
        }
    }
    Some(total + pending.unwrap_or(0))
}

fn season_from_relative_path(relative_path: &Path) -> Option<u32> {
    relative_path
        .parent()?
        .components()
        .rev()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(season_from_directory_name)
}

// Use matched TMDB metadata, not source directory category labels.
fn library_category(metadata: Option<&Value>) -> (&'static str, &'static str) {
    let Some(metadata) = metadata else {
        return ("", "TMDB 未匹配、未配置密钥或请求失败");
    };
    let media_type = metadata["mediaType"].as_str().unwrap_or("");
    if !matches!(media_type, "movie" | "tv") {
        return ("", "TMDB 缺少有效的媒体类型");
    }
    let Some(genres) = metadata_genre_ids(metadata) else {
        return ("", "TMDB 缺少题材信息，无法排除动漫或综艺");
    };
    if genres.is_empty() {
        return ("", "TMDB 题材为空或格式无效");
    }
    let has_genre = |id| genres.contains(&id);
    if has_genre(16) {
        ("动漫", "TMDB Animation (16)")
    } else if media_type == "tv" && (has_genre(10764) || has_genre(10767)) {
        ("综艺", "TMDB Reality/Talk (10764/10767)")
    } else if media_type == "movie" {
        ("电影", "TMDB movie")
    } else {
        ("剧集", "TMDB tv")
    }
}
fn metadata_genre_ids(metadata: &Value) -> Option<Vec<u64>> {
    let mut found_array = false;
    for key in ["genreIds", "genre_ids", "genres"] {
        let Some(items) = metadata.get(key).and_then(Value::as_array) else {
            continue;
        };
        found_array = true;
        let ids: Vec<_> = items
            .iter()
            .filter_map(|item| {
                item.as_u64()
                    .or_else(|| item["id"].as_u64())
                    .or_else(|| item.as_str().and_then(|value| value.parse().ok()))
            })
            .collect();
        if !ids.is_empty() {
            return Some(ids);
        }
    }
    found_array.then_some(Vec::new())
}
fn library_destination_dir(
    root: &Path,
    media: &MediaName,
    metadata: Option<&Value>,
    unknown: &str,
) -> PathBuf {
    let (category, _) = library_category(metadata);
    if library_category(metadata).0.is_empty() {
        return root.join(unknown).join(&media.source_parent);
    }
    let title = metadata
        .and_then(|v| v["title"].as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(&media.title);
    let year = metadata
        .and_then(|v| v["year"].as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(&media.year);
    let id = metadata.and_then(|v| v["id"].as_u64());
    let folder = format!(
        "{} ({}){}",
        if clean(title).is_empty() {
            "未识别媒体".into()
        } else {
            clean(title)
        },
        if year.is_empty() {
            "未知年份"
        } else {
            year
        },
        id.map(|id| format!(" {{tmdb-{id}}}")).unwrap_or_default()
    );
    let mut destination = root
        .join(if category.is_empty() {
            unknown
        } else {
            category
        })
        .join(folder);
    let kind = metadata
        .and_then(|v| v["mediaType"].as_str())
        .unwrap_or(&media.kind);
    if kind == "tv" {
        destination.push(format!("Season {}", media.season.unwrap_or(1)));
    }
    destination
}
fn library_file_stem(
    media: &MediaName,
    metadata: Option<&Value>,
    episode_name: &str,
    original: &str,
) -> String {
    if library_category(metadata).0.is_empty() {
        return clean(
            Path::new(original)
                .file_stem()
                .and_then(|v| v.to_str())
                .unwrap_or(original),
        );
    }
    let title = metadata
        .and_then(|v| v["title"].as_str())
        .unwrap_or(&media.title);
    let year = metadata
        .and_then(|v| v["year"].as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(&media.year);
    let mut parts = vec![clean(title)];
    if !year.is_empty() {
        parts.push(year.into());
    }
    let kind = metadata
        .and_then(|v| v["mediaType"].as_str())
        .unwrap_or(&media.kind);
    if kind == "tv" {
        if let Some(episode) = media.episode {
            parts.push(format!("S{:02}E{episode:02}", media.season.unwrap_or(1)));
            parts.push(if episode_name.is_empty() {
                format!("第{episode}集")
            } else {
                clean(episode_name)
            });
        } else {
            parts.push(clean(
                Path::new(original)
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or(original),
            ));
        }
    }
    // Preserve real technical tags without inventing stream information.
    let tags = Regex::new(r"(?i)2160p|1080p|720p|480p|4k|8k|SDR|HDR10\+?|HDR|DoVi|H\.?26[45]|HEVC|x26[45]|AV1|\d+(?:\.\d+)?fps|AAC(?:[ .]+[0-9]\.[0-9])?|FLAC|DTS").unwrap();
    parts.extend(tags.find_iter(original).map(|m| m.as_str().to_string()));
    clean(
        &parts
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join("."),
    )
}
#[cfg(test)]
fn write_library_entry(
    out: &Path,
    link: &str,
    _media: &MediaName,
    _metadata: Option<&Value>,
    _episode_name: &str,
    _settings: &TmdbSettings,
) -> Result<(), String> {
    fs::create_dir_all(out.parent().ok_or("媒体文件目录无效")?).map_err(|e| e.to_string())?;
    fs::write(out, format!("{link}\n")).map_err(|e| e.to_string())
}

fn import_media_name(file_name: &str, relative_path: &Path) -> MediaName {
    let mut parsed = parse_media(file_name);
    parsed.source_parent = relative_path
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(v) => Some(v),
            _ => None,
        })
        .collect();
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    if parsed.episode.is_none()
        && stem.chars().all(|c| c.is_ascii_digit())
        && !stem.is_empty()
        && stem.len() <= 3
    {
        parsed.episode = stem.parse().ok();
        parsed.title.clear();
    }
    if parsed.title.is_empty() {
        if let Some(directory_name) = relative_path.parent().and_then(|parent| {
            parent
                .components()
                .rev()
                .filter_map(|component| component.as_os_str().to_str())
                .filter(|name| !["动漫", "电影", "剧集", "综艺", "未分类"].contains(name))
                .map(parse_media)
                .find(|name| !name.title.is_empty())
        }) {
            parsed.title = directory_name.title;
            if parsed.year.is_empty() {
                parsed.year = directory_name.year;
            }
        }
    }
    // Episode filenames are often romanized or only numbered; the enclosing
    // series directory usually carries the title that TMDB can match.
    if parsed.episode.is_some() {
        let parent_candidates: Vec<_> = relative_path
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .rev()
            .filter_map(|component| component.as_os_str().to_str())
            .filter_map(|name| {
                if ["动漫", "电影", "剧集", "综艺", "未分类", "合集", "动画", "电视剧", "TV", "BD", "BDrip", "视频"].contains(&name) || Regex::new(r"(?i)^(?:动漫|动画|电影|剧集|综艺|影视|资源|视频)?(?:合集|全集|complete|collection)(?:\s*第[0-9一二三四五六七八九十]+季)?$").unwrap().is_match(name) {
                    return None;
                }
                let candidate = parse_media(name);
                (!candidate.title.is_empty()).then_some((
                    season_from_directory_name(name).is_some(),
                    candidate.title,
                    candidate.year,
                ))
            })
            .collect();
        if let Some((_, title, year)) = parent_candidates.first() {
            parsed.title = title.clone();
            if !year.is_empty() {
                parsed.year = year.clone();
            }
        }
    }
    if parsed.title.is_empty() {
        parsed.title = relative_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|v| v.to_str())
            .unwrap_or("未识别媒体")
            .to_string();
    }
    let explicit_season = Regex::new(r"(?i)S\d{1,3}E\d{1,4}|第\s*\d+\s*季")
        .unwrap()
        .is_match(file_name);
    if !explicit_season {
        if let Some(directory_season) = season_from_relative_path(relative_path) {
            parsed.season = Some(directory_season);
        }
    }
    if parsed.episode.is_some() || parsed.season.is_some() {
        parsed.season = Some(parsed.season.unwrap_or(1));
        parsed.kind = "tv".into();
    }
    parsed
}
fn is_video(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "iso"
            | "mp4"
            | "mkv"
            | "avi"
            | "mov"
            | "m2ts"
            | "ts"
            | "wmv"
            | "flv"
            | "rmvb"
            | "webm"
            | "m4v"
            | "rm"
            | "mpg"
            | "mpeg"
    )
}
fn register_proxy_files(
    state: &ServerState,
    share_key: &str,
    password: &str,
    videos: &[(RemoteFile, PathBuf)],
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut data = state.data.lock().map_err(|_| "状态锁失败")?;
    let share_id = data
        .proxy_shares
        .iter()
        .find(|share| share.share_key == share_key)
        .map(|share| share.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let password_key = format!("proxy-share-password-{share_id}");
    store_secret(&password_key, password)?;
    if let Some(share) = data
        .proxy_shares
        .iter_mut()
        .find(|share| share.id == share_id)
    {
        share.password_key = password_key;
    } else {
        data.proxy_shares.push(ProxyShare {
            id: share_id.clone(),
            share_key: share_key.to_string(),
            password_key,
        });
    }
    let mut result = std::collections::HashMap::new();
    for (file, relative_path) in videos {
        let relative_path = relative_path.to_string_lossy().replace('\\', "/");
        let lookup = format!("{}\n{}", file.id, relative_path);
        let existing = data
            .proxy_files
            .iter()
            .find(|item| {
                item.share_id == share_id
                    && item.file_id == file.id
                    && item.relative_path == relative_path
            })
            .map(|item| item.id.clone());
        let id = existing.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        if let Some(item) = data.proxy_files.iter_mut().find(|item| item.id == id) {
            item.file = file.clone();
        } else {
            data.proxy_files.push(ProxyFile {
                id: id.clone(),
                share_id: share_id.clone(),
                file_id: file.id.clone(),
                relative_path,
                file: file.clone(),
            });
        }
        result.insert(lookup, id);
    }
    drop(data);
    persist(state)?;
    Ok(result)
}

fn percent_decode_path(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

enum ProxyReply {
    Redirect(String),
    Json(Vec<u8>),
}

fn proxy_listing(
    share_key: &str,
    password: &str,
    parent: &str,
) -> Result<Vec<u8>, (u16, &'static str)> {
    let files =
        list_share(share_key, password, parent).map_err(|error| proxy_listing_error(&error))?;
    serialize_proxy_listing(&files).map_err(|_| (500, "failed to serialize file list"))
}

fn proxy_listing_error(error: &str) -> (u16, &'static str) {
    let normalized = error.to_ascii_lowercase();
    if error.contains("失效") || error.contains("取消") {
        (410, "share expired")
    } else if ["密码", "提取码", "访问码", "password", "passcode"]
        .iter()
        .any(|term| normalized.contains(&term.to_ascii_lowercase()))
    {
        (403, "invalid password")
    } else if error.contains("不存在") || normalized.contains("not found") {
        (404, "share or folder not found")
    } else {
        (502, "share service unavailable")
    }
}

fn proxy_direct_link(
    share_key: &str,
    file: &RemoteFile,
) -> Result<ProxyReply, (u16, &'static str)> {
    let token = secret("drive-pan123").ok_or((503, "123 drive login required"))?;
    cached_playback_link(share_key, file, &token)
        .map(ProxyReply::Redirect)
        .map_err(|error| {
            if error.contains("失效") || error.contains("取消") {
                (410, "share expired")
            } else if error.contains("不存在") || error.contains("not found") {
                (404, "file not found")
            } else if error.contains("登录") || error.contains("token") {
                (503, "123 drive login expired")
            } else {
                (502, "failed to refresh media link")
            }
        })
}

fn serialize_proxy_listing(files: &[RemoteFile]) -> Result<Vec<u8>, serde_json::Error> {
    let response: Vec<Value> = files
        .iter()
        .map(|file| {
            json!({
                "fileId": file.id,
                "name": file.name,
                "type": if file.is_dir { "directory" } else { "file" },
                "size": file.size,
                "s3KeyFlag": file.s3,
                "etag": file.etag,
            })
        })
        .collect();
    serde_json::to_vec(&response)
}

fn proxy_file_matches(file: &ProxyFile, target: &str) -> bool {
    file.file_id == target || file.relative_path == target
}

fn proxy_file_target_path(segments: &[String]) -> Option<String> {
    if segments.len() < 3 || segments[0] != "123pan" {
        return None;
    }
    percent_decode_path(&segments[2..].join("/"))
}

fn proxy_lookup(
    state: &ServerState,
    request_target: &str,
) -> Result<ProxyReply, (u16, &'static str)> {
    let parsed = reqwest::Url::parse(&format!("http://localhost{request_target}"))
        .map_err(|_| (400, "invalid request"))?;
    let segments: Vec<String> = parsed
        .path_segments()
        .ok_or((400, "invalid request"))?
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();

    // This stable, unguessable URL is what generated STRM files use.
    if segments.len() == 2 && segments[0] == "media" {
        let (share_key, file) = {
            let data = state.data.lock().map_err(|_| (500, "internal error"))?;
            let item = data
                .proxy_files
                .iter()
                .find(|item| item.id == segments[1])
                .ok_or((404, "media not found"))?;
            let share = data
                .proxy_shares
                .iter()
                .find(|share| share.id == item.share_id)
                .ok_or((404, "share not found"))?;
            (share.share_key.clone(), item.file.clone())
        };
        return proxy_direct_link(&share_key, &file);
    }

    if segments.len() >= 2 && segments[0] == "123pan" {
        let share_key = segments[1].clone();
        let (password_key, registered_file) = {
            let data = state.data.lock().map_err(|_| (500, "internal error"))?;
            let share = data
                .proxy_shares
                .iter()
                .find(|share| share.share_key == share_key)
                .ok_or((404, "share not found"))?;
            let file = if let Some(target) = proxy_file_target_path(&segments) {
                data.proxy_files
                    .iter()
                    .find(|item| item.share_id == share.id && proxy_file_matches(item, &target))
                    .map(|item| item.file.clone())
            } else {
                None
            };
            (share.password_key.clone(), file)
        };
        let supplied_password = parsed
            .query_pairs()
            .find(|(key, _)| key == "pwd")
            .map(|(_, value)| value.into_owned())
            .ok_or((403, "password required"))?;
        let stored_password = secret(&password_key).unwrap_or_default();
        if supplied_password != stored_password {
            return Err((403, "invalid password"));
        }
        if let Some(file) = registered_file {
            return proxy_direct_link(&share_key, &file);
        }
        // With no file match, treat the final component as a directory ID and
        // return that directory's current JSON listing as described by the API.
        let parent = if segments.len() == 2 {
            "0".to_string()
        } else {
            let target = proxy_file_target_path(&segments).ok_or((400, "invalid file path"))?;
            if target.contains('/')
                || target.is_empty()
                || !target.bytes().all(|b| b.is_ascii_digit())
            {
                return Err((404, "file not found"));
            }
            target
        };
        return proxy_listing(&share_key, &stored_password, &parent).map(ProxyReply::Json);
    }
    Err((404, "not found"))
}
