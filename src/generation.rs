// Selected sidecar formats are downloaded incrementally; videos keep stable proxy URLs.
fn metadata_destination(
    rel: &Path,
    root: &Path,
    unclassified: &str,
    destinations: &HashMap<PathBuf, PathBuf>,
) -> PathBuf {
    let name = rel
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("metadata");
    let stem = rel.file_stem().and_then(|v| v.to_str()).unwrap_or("");
    let matched = destinations
        .iter()
        .filter(|(source, _)| source.parent() == rel.parent())
        .filter_map(|(source, out)| {
            let video_stem = source.file_stem()?.to_str()?;
            let tail = stem.strip_prefix(video_stem)?;
            (tail.is_empty() || tail.starts_with(['.', '_', '-', ' '])).then_some((
                video_stem.len(),
                out,
                tail,
            ))
        })
        .max_by_key(|(len, _, _)| *len);
    if let Some((_, out, tail)) = matched {
        let ext = rel.extension().and_then(|v| v.to_str()).unwrap_or("");
        return out.with_file_name(format!(
            "{}{tail}.{ext}",
            out.file_stem().unwrap().to_string_lossy()
        ));
    }
    // Folder artwork and subtitles without a matching basename retain their original names.
    let folders: std::collections::HashSet<_> = destinations
        .iter()
        .filter(|(source, _)| source.parent() == rel.parent())
        .filter_map(|(_, out)| out.parent())
        .collect();
    if folders.len() == 1 {
        return folders.iter().next().unwrap().join(name);
    }
    root.join(unclassified).join(rel)
}
fn download_metadata(
    _state: &ServerState,
    key: &str,
    file: &RemoteFile,
    rel: &Path,
    root: &Path,
    guard: &Path,
    unclassified: &str,
    destinations: &HashMap<PathBuf, PathBuf>,
    _settings: &TmdbSettings,
) -> Result<(PathBuf, bool), String> {
    use std::io::Read;
    const LIMIT: u64 = 64 * 1024 * 1024;
    let out = metadata_destination(rel, root, unclassified, destinations);
    let parent = create_storage_dir(guard, out.parent().ok_or("元数据目录无效")?)?;
    let out = parent.join(out.file_name().ok_or("文件名无效")?);
    // symlink_metadata also detects dangling links; never follow one when writing.
    if fs::symlink_metadata(&out).is_ok() {
        return Ok((out, true));
    }
    if file.size > LIMIT {
        return Err("元数据超过 64 MB 下载限制".into());
    }
    let token = secret("drive-pan123").ok_or("123 云盘凭证不存在")?;
    let url = direct_link(key, file, &token)?;
    let response = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| "下载客户端初始化失败")?
        .get(url)
        .header("User-Agent", UA)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| "元数据下载失败，请检查网盘登录态和网络")?;
    let mut bytes = Vec::new();
    response
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "元数据下载中断")?;
    if bytes.len() as u64 > LIMIT {
        return Err("元数据超过 64 MB 下载限制".into());
    }
    if file.size > 0 && bytes.len() as u64 != file.size {
        return Err("元数据长度与分享文件不符，未保存".into());
    }
    let temp = parent.join(format!(".metadata-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<(), String> {
        let mut handle = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        handle.write_all(&bytes).map_err(|e| e.to_string())?;
        handle.sync_all().map_err(|e| e.to_string())?;
        drop(handle);
        // Hard-link publication is atomic and never replaces an existing file.
        fs::hard_link(&temp, &out).map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result?;
    Ok((out, false))
}
fn reconcile_generated(
    state: &ServerState,
    key: &str,
    root: &Path,
    generated: &[Value],
    keep_versions: bool,
) -> Result<(), String> {
    let manifest = state.dir.join(format!(
        "strm-{}.json",
        token_hash(&format!("{key}\n{}", root.display()))
    ));
    let previous: Vec<Value> = fs::read(&manifest)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if keep_versions {
        let mut all=generated.to_vec();
        for item in previous {if !all.iter().any(|v|v["path"]==item["path"]) {all.push(item);}}
        return atomic_json(&manifest,&all);
    }
    for item in previous {
        if item["preserveOnly"].as_bool() == Some(true) { continue; }
        let Some(path) = item["path"].as_str() else {
            continue;
        };
        if generated.iter().any(|v| v["path"].as_str() == Some(path)) {
            continue;
        }
        let Ok(path) = storage_path(state, path) else {
            continue;
        };
        if !path.starts_with(root)
            || path
                .extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case("strm"))
        {
            continue;
        }
        // Remove only our previous output if its content was not edited by the user.
        if fs::read_to_string(&path)
            .ok()
            .is_some_and(|v| Some(v.trim()) == item["link"].as_str())
        {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
    }
    atomic_json(&manifest, &generated)
}
