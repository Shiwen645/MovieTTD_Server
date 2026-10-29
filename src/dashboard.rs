// Dashboard metadata lives in the service data directory, never beside STRM files.
static LIBRARY_INDEX_LOCK: Mutex<()> = Mutex::new(());
fn read_library_index(dir: &Path) -> HashMap<PathBuf, Value> {
    fs::read(dir.join("library-index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn record_library_item_in(
    data_dir: &Path,
    out: &Path,
    media: &MediaName,
    metadata: Option<&Value>,
) -> Result<(), String> {
    let Some(metadata) = metadata else {
        return Ok(());
    };
    let category = library_category(Some(metadata)).0;
    if category.is_empty() {
        return Ok(());
    }
    let folder = if metadata["mediaType"].as_str().unwrap_or(&media.kind) == "tv" {
        out.parent().and_then(Path::parent)
    } else {
        out.parent()
    }
    .ok_or("媒体目录无效")?;
    let _guard = LIBRARY_INDEX_LOCK.lock().map_err(|_| "媒体索引锁失败")?;
    let mut index = read_library_index(data_dir);
    let added = index
        .get(folder)
        .and_then(|v| v["addedAt"].as_i64())
        .unwrap_or_else(now);
    index.insert(
        folder.to_path_buf(),
        json!({"metadata":metadata,"category":category,"addedAt":added}),
    );
    atomic_json(&data_dir.join("library-index.json"), &index)
}
fn relocate_library_index(data_dir: &Path, moves: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let _guard = LIBRARY_INDEX_LOCK.lock().map_err(|_| "媒体索引锁失败")?;
    let index = read_library_index(data_dir);
    let mut next = HashMap::new();
    for (path, value) in index {
        let path = moves
            .iter()
            .find_map(|(from, to)| path.strip_prefix(from).ok().map(|tail| to.join(tail)))
            .unwrap_or(path);
        next.insert(path, value);
    }
    atomic_json(&data_dir.join("library-index.json"), &next)
}
fn nfo_text(xml: &str, tag: &str) -> String {
    let raw = xml
        .split_once(&format!("<{tag}>"))
        .and_then(|(_, s)| s.split_once(&format!("</{tag}>")))
        .map(|(s, _)| s)
        .unwrap_or("");
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
fn dashboard(state: &ServerState) -> Result<Value, String> {
    let image_base = state
        .data
        .lock()
        .map_err(|_| "配置锁失败")?
        .tmdb
        .clone()
        .unwrap_or_else(defaults)
        .image_base_url;
    let roots = state
        .storage_roots
        .lock()
        .map_err(|_| "保存目录锁失败")?
        .clone();
    let mut stack = roots.clone();
    let mut files = Vec::new();
    let mut markers: HashMap<PathBuf, Value> = read_library_index(&state.dir);
    let mut warnings = Vec::new();
    let mut visited = 0;
    let mut visited_paths = std::collections::HashSet::new();
    while let Some(dir) = stack.pop() {
        if !visited_paths.insert(dir.clone()) {
            continue;
        }
        visited += 1;
        if visited > 100000 {
            return Err("媒体库目录超过遍历上限".into());
        }
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!("无法读取目录：{}", dir.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(v) => v,
                Err(_) => {
                    warnings.push("部分目录条目读取失败".into());
                    continue;
                }
            };
            let kind = entry.file_type().map_err(|_| "读取文件类型失败")?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            // Windows junctions can masquerade as directories; never escape the configured root.
            if kind.is_dir() {
                if let Ok(canonical) = fs::canonicalize(&path) {
                    if roots.iter().any(|root| canonical.starts_with(root))
                        && !stack.contains(&canonical)
                    {
                        stack.push(canonical);
                    }
                }
            } else if kind.is_file() {
                if path
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("strm"))
                {
                    files.push(path.clone());
                }
                if entry.file_name() == ".moviettd.json" {
                    if let Some(v) = fs::read(&path)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    {
                        markers.entry(dir.clone()).or_insert(v);
                    }
                }
            }
        }
    }
    let file_count = files.len();
    let mut groups: HashMap<PathBuf, Value> = HashMap::new();
    for file in files {
        let marker = file
            .ancestors()
            .skip(1)
            .take_while(|p| roots.iter().any(|root| p.starts_with(root)))
            .find_map(|p| markers.get(p).map(|v| (p.to_path_buf(), v.clone())));
        let (folder, item) = if let Some((folder, marker)) = marker {
            let m = &marker["metadata"];
            (
                folder,
                json!({"title":m["title"],"year":m["year"],"mediaType":m["mediaType"],"category":marker["category"],"posterPath":m["poster"],"addedAt":marker["addedAt"],"tmdbId":m["id"]}),
            )
        } else {
            // Older libraries have NFO files but no sidecar. Read their metadata without making
            // a TMDB request for every episode or changing admission timestamps during a scan.
            let show = file
                .ancestors()
                .skip(1)
                .take_while(|p| roots.iter().any(|root| p.starts_with(root)))
                .find(|p| p.join("tvshow.nfo").is_file());
            let folder = show.unwrap_or_else(|| file.parent().unwrap());
            if groups.contains_key(folder) {
                continue;
            }
            let nfo = if show.is_some() {
                folder.join("tvshow.nfo")
            } else {
                file.with_extension("nfo")
            };
            let xml = fs::read_to_string(nfo).unwrap_or_default();
            let category = folder
                .strip_prefix(
                    roots
                        .iter()
                        .find(|root| folder.starts_with(root))
                        .unwrap_or(&state.library_root),
                )
                .ok()
                .into_iter()
                .flat_map(Path::components)
                .filter_map(|c| c.as_os_str().to_str())
                .find(|c| ["动漫", "电影", "剧集", "综艺"].contains(c))
                .unwrap_or("");
            if category.is_empty() {
                continue;
            }
            let added = fs::metadata(folder)
                .ok()
                .and_then(|m| m.created().or_else(|_| m.modified()).ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let title = nfo_text(&xml, "title");
            let mut poster = nfo_text(&xml, "thumb");
            if poster.is_empty() && show.is_some() {
                poster = nfo_text(
                    &fs::read_to_string(file.with_extension("nfo")).unwrap_or_default(),
                    "thumb",
                );
            }
            (
                folder.to_path_buf(),
                json!({"title":if title.is_empty(){folder.file_name().unwrap_or_default().to_string_lossy().into_owned()}else{title},"year":nfo_text(&xml,"year"),"category":category,"mediaType":if show.is_some(){"tv"}else{"movie"},"posterPath":poster.split("/w500").nth(1).unwrap_or(""),"addedAt":added}),
            )
        };
        let mut item = item;
        item["directory"] = json!(folder.to_string_lossy());
        groups.entry(folder).or_insert(item);
    }
    let mut all: Vec<Value> = groups.into_values().collect();
    all.sort_by(|a, b| {
        b["addedAt"]
            .as_i64()
            .cmp(&a["addedAt"].as_i64())
            .then_with(|| a["title"].as_str().cmp(&b["title"].as_str()))
    });
    let series_count = all.iter().filter(|v| v["mediaType"] == "tv").count();
    let movie_count = all.iter().filter(|v| v["mediaType"] == "movie").count();
    let categories: Vec<Value> = ["动漫", "电影", "剧集", "综艺"]
        .into_iter()
        .filter_map(|category| {
            let items: Vec<Value> = all
                .iter()
                .filter(|v| v["category"] == category)
                .map(|v| {
                    let mut v = v.clone();
                    let poster = v["posterPath"].as_str().unwrap_or("");
                    v["posterUrl"] = json!(if poster.starts_with('/') {
                        format!("{}/w342{poster}", image_base.trim_end_matches('/'))
                    } else {
                        String::new()
                    });
                    v
                })
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(json!({"name":category,"total":items.len(),"items":items}))
            }
        })
        .collect();
    Ok(
        json!({"seriesCount":series_count,"movieCount":movie_count,"fileCount":file_count,"categories":categories,"warnings":warnings}),
    )
}
