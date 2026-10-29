// Source sizes and identity are centralized; a STRM file's byte size is never media size.
static RESOURCE_LOCK: Mutex<()> = Mutex::new(());
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceRecord {
    identity: String,
    source_size: u64,
    link: String,
}
struct ResourceWrite {
    path: PathBuf,
    link: String,
    written: bool,
    preserve_only: bool,
    reason: String,
}
fn resource_identity(media: &MediaName, metadata: Option<&Value>, out: &Path) -> String {
    if let Some(meta) = metadata {
        if let Some(id) = meta["id"].as_u64() {
            if meta["mediaType"].as_str().unwrap_or(&media.kind) == "movie" {
                return format!("movie:{id}");
            }
            if let Some(ep) = media.episode {
                return format!("tv:{id}:{}:{ep}", media.season.unwrap_or(1));
            }
        }
    }
    // Unmatched resources must not merge across titles or seasons.
    format!("path:{}", out.to_string_lossy())
}
fn metadata_belongs_to(attachment: &Path, source: &Path) -> bool {
    if attachment.parent() != source.parent() {
        return false;
    }
    let a = attachment
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let b = source.file_stem().and_then(|v| v.to_str()).unwrap_or("");
    a.strip_prefix(b)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(['.', '_', '-', ' ']))
}
fn write_resource(
    state: &ServerState,
    root: &Path,
    target: &Path,
    link: &str,
    file: &RemoteFile,
    media: &MediaName,
    metadata: Option<&Value>,
    policy: &str,
) -> Result<ResourceWrite, String> {
    let _guard = RESOURCE_LOCK.lock().map_err(|_| "资源索引锁失败")?;
    let index_path = state.dir.join("resource-index.json");
    let mut index: HashMap<PathBuf, ResourceRecord> = if index_path.exists() {
        serde_json::from_slice(&fs::read(&index_path).map_err(|e| e.to_string())?)
            .map_err(|_| "资源索引损坏，已停止覆盖")?
    } else {
        HashMap::new()
    };
    let identity = resource_identity(media, metadata, target);
    let mut existing: Vec<PathBuf> = index
        .iter()
        .filter(|(p, r)| r.identity == identity && p.starts_with(root) && p.is_file())
        .map(|(p, _)| p.clone())
        .collect();
    // Recognize legacy files in the same TMDB destination without guessing their source size.
    if let Some(dir) = target.parent() {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let p = entry.map_err(|e| e.to_string())?.path();
            if !p
                .extension()
                .is_some_and(|v| v.eq_ignore_ascii_case("strm"))
                || existing.contains(&p)
            {
                continue;
            }
            let name = p.file_name().and_then(|v| v.to_str()).unwrap_or("");
            let same = if identity.starts_with("movie:") {
                true
            } else if identity.starts_with("tv:") {
                let other = parse_media(name);
                other.episode == media.episode
                    && other.season.unwrap_or(1) == media.season.unwrap_or(1)
            } else {
                p == target
            };
            if same {
                existing.push(p);
            }
        }
    }
    existing.sort();
    let same_link = existing
        .iter()
        .find(|p| fs::read_to_string(p).ok().is_some_and(|s| s.trim() == link))
        .cloned();
    if let Some(path) = same_link {
        index.insert(
            path.clone(),
            ResourceRecord {
                identity,
                source_size: file.size,
                link: link.into(),
            },
        );
        atomic_json(&index_path, &index)?;
        return Ok(ResourceWrite {
            path,
            link: link.into(),
            written: false,
            preserve_only: false,
            reason: "同一源文件已经入库".into(),
        });
    }
    if !existing.is_empty() {
        let protected = existing.iter().any(|p| {
            index.get(p).is_none_or(|r| {
                fs::read_to_string(p)
                    .ok()
                    .is_none_or(|s| s.trim() != r.link)
            })
        });
        let unknown = existing
            .iter()
            .any(|p| index.get(p).is_none_or(|r| r.source_size == 0));
        let largest = existing
            .iter()
            .filter_map(|p| index.get(p).map(|r| r.source_size))
            .max()
            .unwrap_or(0);
        let reason = if policy == "skip" {
            Some("已存在同一部电影或同一集，按规则跳过")
        } else if policy == "larger" && unknown {
            Some("已有资源缺少原视频大小记录，保留并跳过")
        } else if policy == "larger" && (file.size == 0 || file.size <= largest) {
            Some("新视频不大于已有视频，保留并跳过")
        } else if policy != "keep" && protected {
            Some("已有 STRM 未被本服务记录或内容已被编辑，保留并跳过")
        } else {
            None
        };
        if let Some(reason) = reason {
            let path = existing[0].clone();
            let old = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            return Ok(ResourceWrite {
                path,
                link: old.trim().into(),
                written: false,
                preserve_only: true,
                reason: reason.into(),
            });
        }
    }
    let mut out = target.to_path_buf();
    if policy == "keep" && out.exists() {
        let stem = out.file_stem().unwrap().to_string_lossy();
        out = out.with_file_name(format!("{stem}.{}.strm", &token_hash(link)[..10]));
    }
    // Never follow a symlink or overwrite a colliding unmanaged file.
    if fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("目标 STRM 是符号链接".into());
    }
    if out.exists() && !existing.contains(&out) {
        return Err("目标路径已被其他资源占用".into());
    }
    let temp = out.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    fs::write(&temp, format!("{link}\n")).map_err(|e| e.to_string())?;
    let publish = if existing.contains(&out) {
        fs::rename(&temp, &out)
    } else {
        fs::hard_link(&temp, &out)
    };
    if let Err(e) = publish {
        let _ = fs::remove_file(&temp);
        return Err(e.to_string());
    }
    let _ = fs::remove_file(&temp);
    index.insert(
        out.clone(),
        ResourceRecord {
            identity,
            source_size: file.size,
            link: link.into(),
        },
    );
    atomic_json(&index_path, &index)?;
    if policy != "keep" {
        for old in existing {
            if old == out {
                continue;
            }
            if let Some(record) = index.get(&old) {
                if fs::read_to_string(&old)
                    .ok()
                    .is_some_and(|s| s.trim() == record.link)
                {
                    fs::remove_file(&old)
                        .map_err(|e| format!("新 STRM 已写入，但旧版本清理失败：{e}"))?;
                    index.remove(&old);
                }
            }
        }
        atomic_json(&index_path, &index)?;
    }
    Ok(ResourceWrite {
        path: out,
        link: link.into(),
        written: true,
        preserve_only: false,
        reason: if policy == "larger" {
            "按原视频大小比较后写入"
        } else {
            "已写入 STRM"
        }
        .into(),
    })
}
