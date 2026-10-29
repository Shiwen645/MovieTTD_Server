#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn signed_user_info_request_matches_yunx() {
        let client = Client::new();
        let request = pan123_validation_request(
            &client,
            "synthetic-token",
            "0123456789abcdef0123456789abcdef",
        )
        .build()
        .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://yun.123pan.cn/b/api/user/info"
        );
        let headers = request.headers();
        assert_eq!(headers["authorization"], "Bearer synthetic-token");
        assert_eq!(headers["platform"], "web");
        assert_eq!(headers["app-version"], "3");
        assert_eq!(headers["loginuuid"], "0123456789abcdef0123456789abcdef");
        let key = headers["auth-key"].to_str().unwrap();
        let value = headers["auth-value"].to_str().unwrap();
        let parts: Vec<_> = value.split('-').collect();
        assert_eq!(parts.len(), 3);
        let raw = format!("{}|{}|/b/api/user/info|web|3|{key}", parts[0], parts[1]);
        assert_eq!(parts[2], format!("{:x}", crc32fast::hash(raw.as_bytes())));
        assert!(parts[1].parse::<u32>().unwrap() < 10_000_000);
    }

    #[test]
    fn pan123_accepts_only_user_info_success_with_identity() {
        assert!(
            validate_account_response("pan123", &json!({"code":0,"data":{"Nickname":"测试"}}))
                .is_ok()
        );
        for value in [
            json!({"code":200,"data":{"Nickname":"测试"}}),
            json!({"code":0,"data":{}}),
            json!({"code":0,"data":{"Nickname":" "}}),
            json!({"code":401,"message":"sensitive-response"}),
        ] {
            let error = validate_account_response("pan123", &value).unwrap_err();
            assert!(!error.contains("sensitive-response"));
        }
    }

    #[test]
    fn cookie_providers_require_success_and_identity() {
        for provider in ["quark", "uc"] {
            assert!(
                validate_account_response(
                    provider,
                    &json!({"success":true,"code":"OK","data":{"nickname":"测试"}})
                )
                .is_ok()
            );
            assert!(
                validate_account_response(
                    provider,
                    &json!({"success":false,"data":{"nickname":"测试"}})
                )
                .is_err()
            );
            assert!(
                validate_account_response(provider, &json!({"success":true,"data":{}})).is_err()
            );
        }
    }

    #[test]
    fn cookie_detection_checks_exact_names_and_nonempty_values() {
        assert!(cookie_is_plausible("__pus=a; __puus=b==; x=c"));
        assert!(!cookie_is_plausible("not__pus=a; __puus=b"));
        assert!(!cookie_is_plausible("__pus=; __puus=b"));
        assert!(!cookie_is_plausible("__pus=a"));
    }

    #[test]
    fn proxy_target_accepts_file_id_and_relative_path() {
        let file = RemoteFile {
            id: "123".into(),
            name: "第一集.mkv".into(),
            is_dir: false,
            size: 1234,
            s3: "s3".into(),
            etag: "etag".into(),
        };
        let entry = ProxyFile {
            id: "opaque".into(),
            share_id: "share".into(),
            file_id: file.id.clone(),
            relative_path: "凡人修仙传/第一集.mkv".into(),
            file,
        };
        assert!(proxy_file_matches(&entry, "123"));
        assert!(proxy_file_matches(&entry, "凡人修仙传/第一集.mkv"));
        assert!(!proxy_file_matches(&entry, "第二集.mkv"));
        let segments = vec![
            "123pan".to_string(),
            "share".to_string(),
            "%E5%87%A1%E4%BA%BA%E4%BF%AE%E4%BB%99%E4%BC%A0%2F%E7%AC%AC%E4%B8%80%E9%9B%86.mkv"
                .to_string(),
        ];
        assert_eq!(
            proxy_file_target_path(&segments).as_deref(),
            Some("凡人修仙传/第一集.mkv")
        );
        assert_eq!(
            percent_decode_path(
                "%E5%87%A1%E4%BA%BA%E4%BF%AE%E4%BB%99%E4%BC%A0%2F%E7%AC%AC%E4%B8%80%E9%9B%86.mkv"
            )
            .as_deref(),
            Some("凡人修仙传/第一集.mkv")
        );
        assert!(percent_decode_path("%E4%ZZ").is_none());
    }

    #[test]
    fn classified_layout_keeps_seasons_and_tmdb_identity() {
        let root = Path::new("D:/Media");
        let metadata = json!({"id":106449,"title":"凡人修仙传","year":"2020","mediaType":"tv","genreIds":[16,10759]});
        for (source, season) in [
            ("动漫/凡人修仙传/Season 02/第01集.mkv", 2),
            ("凡人修仙传/第二季/01.mkv", 2),
            ("凡人修仙传/第01集.mkv", 1),
        ] {
            let media = import_media_name(
                Path::new(source).file_name().unwrap().to_str().unwrap(),
                Path::new(source),
            );
            assert_eq!(media.season, Some(season));
            assert_eq!(media.episode, Some(1));
            assert_eq!(media.title, "凡人修仙传");
            assert_eq!(
                library_destination_dir(root, &media, Some(&metadata), "未分类"),
                root.join("动漫/凡人修仙传 (2020) {tmdb-106449}")
                    .join(format!("Season {season}"))
            );
            assert!(
                library_file_stem(&media, Some(&metadata), "", "第01集.2160p.mkv")
                    .contains(&format!("S{season:02}E01"))
            );
        }
    }
    #[test]
    fn explicit_episode_season_wins_over_generic_source_directory() {
        let media = import_media_name(
            "作品.S02E03.mkv",
            Path::new("作品/Season 1/作品.S02E03.mkv"),
        );
        assert_eq!(media.season, Some(2));
        assert_eq!(media.episode, Some(3));
        assert_eq!(media.title, "作品");
    }

    #[test]
    fn classifications_are_exclusive_and_animation_movies_have_no_season() {
        let cases = [
            ("tv", vec![16, 10764], "动漫"),
            ("movie", vec![16], "动漫"),
            ("tv", vec![10764], "综艺"),
            ("tv", vec![10767], "综艺"),
            ("tv", vec![18], "剧集"),
            ("movie", vec![18], "电影"),
        ];
        for (kind, genres, expected) in cases {
            let metadata =
                json!({"id":12,"title":"测试","year":"2024","mediaType":kind,"genreIds":genres});
            assert_eq!(library_category(Some(&metadata)).0, expected);
            let media = MediaName {
                kind: kind.into(),
                season: Some(2),
                ..Default::default()
            };
            let path =
                library_destination_dir(Path::new("Media"), &media, Some(&metadata), "待确认");
            let base = Path::new("Media")
                .join(expected)
                .join("测试 (2024) {tmdb-12}");
            assert_eq!(
                path,
                if kind == "tv" {
                    base.join("Season 2")
                } else {
                    base
                }
            );
        }
        let detail_shape = json!({"mediaType":"tv","genres":[{"id":16,"name":"Animation"}]});
        assert_eq!(library_category(Some(&detail_shape)).0, "动漫");
    }
    #[test]
    fn unknown_classification_and_configuration_are_safe() {
        let media = parse_media("未知作品.S01E02.mkv");
        assert_eq!(library_category(None).0, "");
        assert_eq!(
            library_category(Some(&json!({"mediaType":"movie","genreIds":[]}))).0,
            ""
        );
        assert_eq!(
            library_destination_dir(Path::new("Media"), &media, None, "待确认"),
            Path::new("Media/待确认")
        );
        assert_eq!(library_category(Some(&json!({"mediaType":"tv"}))).0, "");
        for invalid in [
            "",
            "..",
            "../电影",
            "D:/Media",
            "动漫",
            "电影",
            "综艺",
            "剧集",
            "CON",
        ] {
            assert!(validate_unclassified_folder(invalid).is_err(), "{invalid}");
        }
        assert_eq!(validate_unclassified_folder("待确认").unwrap(), "待确认");
        let old: PersistedState =
            serde_json::from_value(json!({"tmdb":null,"connections":[]})).unwrap();
        assert_eq!(old.unclassified_folder, "未分类");
    }
    #[test]
    fn classified_entry_writes_only_playable_strm() {
        let temp = std::env::temp_dir().join(format!("moviettd-layout-{}", uuid::Uuid::new_v4()));
        let media = import_media_name("第01集.mkv", Path::new("凡人修仙传/第二季/第01集.mkv"));
        let metadata = json!({"id":106449,"title":"凡人修仙传","year":"2020","mediaType":"tv","genreIds":[16]});
        let out = library_destination_dir(&temp, &media, Some(&metadata), "未分类").join(format!(
            "{}.strm",
            library_file_stem(&media, Some(&metadata), "", "第01集.mkv")
        ));
        write_library_entry(
            &out,
            "http://localhost:18765/media/example",
            &media,
            Some(&metadata),
            "",
            &defaults(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "http://localhost:18765/media/example\n"
        );
        assert!(!out.with_extension("nfo").exists());
        assert!(!out.parent().unwrap().parent().unwrap().join("tvshow.nfo").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn unmatched_entry_does_not_create_a_fallback_nfo() {
        let temp = std::env::temp_dir().join(format!("moviettd-unmatched-{}", uuid::Uuid::new_v4()));
        let out = temp.join("未分类/作品/Season 1/01.strm");
        let media = import_media_name("01.mkv", Path::new("作品/Season 1/01.mkv"));
        write_library_entry(
            &out,
            "http://localhost:18765/media/example",
            &media,
            None,
            "",
            &defaults(),
        )
        .unwrap();
        assert!(out.exists());
        assert!(!out.with_extension("nfo").exists());
        assert!(!out.parent().unwrap().parent().unwrap().join("tvshow.nfo").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn proxy_listing_has_stable_file_metadata_shape() {
        let bytes = serialize_proxy_listing(&[RemoteFile {
            id: "123".into(),
            name: "第一集.mkv".into(),
            is_dir: false,
            size: 1234,
            s3: "s3-key".into(),
            etag: "etag-value".into(),
        }])
        .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json[0]["fileId"], "123");
        assert_eq!(json[0]["name"], "第一集.mkv");
        assert_eq!(json[0]["type"], "file");
        assert_eq!(json[0]["size"], 1234);
        assert_eq!(json[0]["s3KeyFlag"], "s3-key");
    }

    #[test]
    fn proxy_listing_maps_share_errors_to_contract_statuses() {
        assert_eq!(
            proxy_listing_error("123分享提取码错误"),
            (403, "invalid password")
        );
        assert_eq!(
            proxy_listing_error("password incorrect"),
            (403, "invalid password")
        );
        assert_eq!(proxy_listing_error("123分享已失效"), (410, "share expired"));
        assert_eq!(
            proxy_listing_error("目录不存在"),
            (404, "share or folder not found")
        );
        assert_eq!(
            proxy_listing_error("请求超时"),
            (502, "share service unavailable")
        );
    }

    #[test]
    fn persisted_proxy_registry_keeps_password_out_of_settings_json() {
        let state = PersistedState {
            tmdb: None,
            strm: StrmSettings::default(),
            connections: vec![],
            proxy_base_url: DEFAULT_PROXY_URL.into(),
            proxy_shares: vec![ProxyShare {
                id: "opaque-share".into(),
                share_key: "Vbq0Vv-z4Kc3".into(),
                password_key: "proxy-share-password-opaque-share".into(),
            }],
            proxy_files: vec![],
            unclassified_folder: default_unclassified_folder(),
        };
        let encoded = serde_json::to_string(&state).unwrap();
        assert!(encoded.contains("Vbq0Vv-z4Kc3"));
        assert!(encoded.contains("proxy-share-password-opaque-share"));
        assert!(!encoded.contains("abcd"));
    }
}

#[cfg(test)]
mod scraping_regressions {
    use super::*;
    #[test]
    fn noisy_bilingual_movies_keep_useful_candidates() {
        for (source, title, year) in [
            ("雷神 Thor 2 30 GB (2011)", "雷神 Thor", "2011"),
            (
                "雷神2：黑暗世界 Thor The Dark World 2 69 GB (2013)",
                "雷神2：黑暗世界 Thor The Dark World",
                "2013",
            ),
            (
                "The Wandering Earth Beyond Special Edition Hybrid DV P8 HDR Vivid DDP 5 1 Atmos (2020)",
                "The Wandering Earth Beyond",
                "2020",
            ),
        ] {
            let parsed = parse_media(source);
            assert_eq!(parsed.title, title, "{source}");
            assert_eq!(parsed.year, year);
            assert_eq!(parsed.kind, "movie");
        }
        assert!(
            title_candidates(&parse_media("雷神 Thor 2 30 GB (2011)").title)
                .contains(&"雷神".into())
        );
        assert_eq!(title_similarity("雷神2", "雷神"), 0.0);
        assert!(title_similarity("The Wandering Earth Beyond", "The Wandering Earth") >= 0.68);
    }
    #[test]
    fn unclassified_shares_preserve_every_season_and_original_filename() {
        let root = Path::new("Media");
        for source in [
            "动漫合集/作品/第一季/01.mkv",
            "动漫合集/作品/第二季/01.mkv",
            "动漫合集/作品/特别篇/OVA 01.mkv",
        ] {
            let relative = Path::new(source);
            let filename = relative.file_name().unwrap().to_str().unwrap();
            let media = import_media_name(filename, relative);
            let destination = library_destination_dir(root, &media, None, "待确认");
            assert_eq!(
                destination,
                root.join("待确认").join(relative.parent().unwrap())
            );
            assert_eq!(
                library_destination_dir(
                    root,
                    &media,
                    Some(&json!({"id":1,"mediaType":"tv"})),
                    "待确认"
                ),
                destination
            );
            assert_eq!(
                library_file_stem(&media, None, "", filename),
                relative.file_stem().unwrap().to_str().unwrap()
            );
        }
    }
    #[test]
    fn quota_requires_valid_numbers_instead_of_zero_placeholders() {
        assert_eq!(
            parse_quota(
                "pan123",
                &json!({"data":{"SpaceUsed":"50","SpacePermanent":100,"SpaceTemp":20}})
            )
            .unwrap()["total"],
            120
        );
        assert!(parse_quota("pan123", &json!({"data":{"Nickname":"Synthetic"}})).is_none());
        assert!(
            parse_quota(
                "quark",
                &json!({"data":{"use_capacity":0,"total_capacity":0}})
            )
            .is_none()
        );
        assert_eq!(
            parse_quota(
                "uc",
                &json!({"data":{"use_capacity":15,"total_capacity":20}})
            )
            .unwrap()["used"],
            15
        );
    }
    #[test]
    fn dashboard_scans_recursively_deduplicates_and_keeps_first_admission_time() {
        let dir = std::env::temp_dir().join(format!("moviettd-dashboard-{}", uuid::Uuid::new_v4()));
        let state = load_state(dir.clone(), dir.join("media")).unwrap();
        let media = parse_media("作品.S01E01.mkv");
        let meta = json!({"id":12,"title":"作品","year":"2020","mediaType":"tv","genreIds":[16],"poster":"/poster.jpg"});
        for season in [1, 2] {
            let output = state
                .library_root
                .join(format!("导入/动漫/作品/Season {season}/01.strm"));
            write_library_entry(
                &output,
                "http://localhost/media/example",
                &media,
                Some(&meta),
                "",
                &defaults(),
            )
            .unwrap();
            record_library_item_in(&state.dir, &output, &media, Some(&meta)).unwrap();
        }
        assert!(!state.library_root.join("导入/动漫/作品/.moviettd.json").exists());
        let mut index=read_library_index(&state.dir);
        index.get_mut(&state.library_root.join("导入/动漫/作品")).unwrap()["addedAt"]=json!(123);
        atomic_json(&state.dir.join("library-index.json"),&index).unwrap();
        record_library_item_in(
            &state.dir,
            &state.library_root.join("导入/动漫/作品/Season 1/01.strm"),
            &media,
            Some(&meta),
        )
        .unwrap();
        let movie = parse_media("电影 1 (2020).mkv");
        for id in 1..=7 {
            let metadata = json!({"id":id,"title":format!("电影 {id}"),"year":"2020","mediaType":"movie","genreIds":[28],"poster":format!("/poster-{id}.jpg")});
            let folder = state.library_root.join(format!("导入/电影/电影 {id} (2020) {{tmdb-{id}}}"));
            let output = folder.join(format!("电影 {id}.strm"));
            write_library_entry(&output, "http://localhost/media/example", &movie, Some(&metadata), "", &defaults()).unwrap();
            record_library_item_in(&state.dir, &output, &movie, Some(&metadata)).unwrap();
        }
        let result = dashboard(&state).unwrap();
        assert_eq!(result["categories"][0]["items"][0]["directory"],state.library_root.join("导入/动漫/作品").to_string_lossy().as_ref());
        assert_eq!(result["seriesCount"], 1);
        assert_eq!(result["movieCount"], 7);
        assert_eq!(result["fileCount"], 9);
        assert_eq!(result["categories"].as_array().unwrap().len(), 2);
        assert_eq!(result["categories"][0]["items"][0]["addedAt"], 123);
        assert!(
            result["categories"][0]["items"][0]["posterUrl"]
                .as_str()
                .unwrap()
                .ends_with("/w342/poster.jpg")
        );
        assert_eq!(result["categories"][1]["name"], "电影");
        assert_eq!(result["categories"][1]["total"], 7);
        assert_eq!(result["categories"][1]["items"].as_array().unwrap().len(), 7);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod tmdb_http_regressions {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn search_cleans_titles_relaxes_bad_year_and_rejects_wrong_sequels() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                let target = first.split_whitespace().nth(1).unwrap();
                let url = reqwest::Url::parse(&format!("http://localhost{target}")).unwrap();
                if url.path() == "/3/movie/103" {
                    let body = json!({"id":103,"title":"流浪地球","release_date":"2019-02-05","genres":[{"id":878,"name":"Science Fiction"}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    continue;
                }
                if url.path() == "/3/movie/535167" {
                    let body = json!({"id":535167,"title":"流浪地球","release_date":"2019-02-05","genres":[{"id":878,"name":"Science Fiction"}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    continue;
                }
                if url.path() == "/3/tv/105/alternative_titles" {
                    let body=json!({"results":[{"title":"Sousou no Frieren","iso_3166_1":"JP"}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();continue;
                }
                if url.path() == "/3/tv/105" {
                    let body=json!({"id":105,"name":"葬送的芙莉莲","first_air_date":"2023-09-29","genres":[{"id":16,"name":"Animation"}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();continue;
                }
                if url.path() == "/3/tv/104" {
                    let body = json!({"id":104,"name":"Mushoku Tensei Jobless Reincarnation","first_air_date":"2021-01-01","genres":[{"id":16,"name":"Animation"}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    continue;
                }
                let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
                let query = params.get("query").cloned().unwrap_or_default();
                let year = params.get("year").cloned().unwrap_or_default();
                seen.lock().unwrap().push((query.clone(), year.clone()));
                let results = match query.as_str() {
                    "雷神" | "Thor" => json!([
                        {"id":999,"title":"雷神3：诸神黄昏","original_title":"Thor Ragnarok","release_date":"2017-01-01","popularity":1000},
                        {"id":101,"title":"雷神","original_title":"Thor","release_date":"2011-01-01","genre_ids":[28]}
                    ]),
                    "雷神2：黑暗世界" | "Thor The Dark World" => json!([
                        {"id":101,"title":"雷神","original_title":"Thor","release_date":"2011-01-01","popularity":1000},
                        {"id":102,"title":"雷神2：黑暗世界","original_title":"Thor: The Dark World","release_date":"2013-01-01","genre_ids":[28]}
                    ]),
                    "The Wandering Earth" if year.is_empty() => json!([
                        {"id":103,"title":"流浪地球","original_title":"The Wandering Earth","release_date":"2019-02-05"}
                    ]),
                    "Mushoku Tensei Jobless Reincarnation" => json!([
                        {"id":104,"name":"Mushoku Tensei Jobless Reincarnation","first_air_date":"2021-01-01"}
                    ]),
                    "Sousou no Frieren" => json!([{"id":105,"name":"葬送的芙莉莲","original_name":"葬送のフリーレン","first_air_date":"2023-09-29","genre_ids":[16]}]),
                    "雷神2" => json!([{"id":101,"title":"雷神","release_date":"2011-01-01"}]),
                    _ => json!([]),
                };
                let body = json!({"results":results}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let mut settings = defaults();
        settings.api_key = "synthetic".into();
        settings.base_url = format!("http://{address}/3");
        let names = [
            "雷神 Thor 2 30 GB (2011)",
            "雷神2：黑暗世界 Thor The Dark World 2 69 GB (2013)",
            "The Wandering Earth Beyond Special Edition Hybrid DV P8 HDR Vivid DDP 5 1 Atmos (2020)",
            "雷神2 (2013)",
        ];
        let matches: Vec<_> = names
            .iter()
            .map(|name| tmdb_match(&parse_media(name), &settings))
            .collect();
        for (index, id) in [101, 102, 103].iter().enumerate() {
            assert_eq!(
                matches[index].as_ref().map(|m| m["id"].as_u64().unwrap()),
                Some(*id),
                "{}",
                names[index]
            );
        }
        assert!(
            matches[3].is_none(),
            "Must not mistake a sequel for the original film"
        );
        assert!(
            queries
                .lock()
                .unwrap()
                .contains(&("The Wandering Earth".into(), String::new()))
        );
        assert_eq!(matches[2].as_ref().unwrap()["year"], "2019");
        let movie = matches[2].as_ref().unwrap();
        assert_eq!(movie["genreIds"], json!([878]));
        assert_eq!(library_category(Some(movie)).0, "电影");
        assert_eq!(
            library_destination_dir(
                Path::new("Media"),
                &parse_media("The Wandering Earth (2019).mkv"),
                Some(movie),
                "待确认"
            ),
            Path::new("Media/电影/流浪地球 (2019) {tmdb-103}")
        );
        let imported = tmdb_match_for_import(
            &import_media_name(
                "The.Wandering.Earth.2019.mkv",
                Path::new("流浪地球 (2019) {tmdb-535167}/The.Wandering.Earth.2019.mkv"),
            ),
            Path::new("流浪地球 (2019) {tmdb-535167}/The.Wandering.Earth.2019.mkv"),
            &settings,
        )
        .unwrap();
        assert_eq!(imported["id"], json!(535167));
        assert_eq!(library_category(Some(&imported)).0, "电影");
        let anime = tmdb_match(
            &parse_media("Mushoku.Tensei.Jobless.Reincarnation.2021.S01E01.mkv"),
            &settings,
        )
        .unwrap();
        assert_eq!(anime["genreIds"], json!([16]));
        assert_eq!(library_category(Some(&anime)).0, "动漫");
        assert_eq!(
            library_destination_dir(
                Path::new("Media"),
                &parse_media("Mushoku.Tensei.S01E01.mkv"),
                Some(&anime),
                "待确认"
            ),
            Path::new("Media/动漫/Mushoku Tensei Jobless Reincarnation (2021) {tmdb-104}/Season 1")
        );
        let alias_match=tmdb_match(&parse_media("Sousou no Frieren - 01 [1080p].mkv"),&settings).unwrap();
        assert_eq!(alias_match["id"],105);assert_eq!(library_category(Some(&alias_match)).0,"动漫");
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
    }

    #[test]
    fn episode_only_names_use_title_bearing_season_parent() {
        for path in [
            "合集/凡人修仙传 第二季/01.mkv",
            "凡人修仙传/Season 02/第01集.mkv",
            "凡人修仙传/第二季/01.mkv",
        ] {
            let relative = Path::new(path);
            let parsed =
                import_media_name(relative.file_name().unwrap().to_str().unwrap(), relative);
            assert_eq!(parsed.title, "凡人修仙传", "{path}");
            assert_eq!(parsed.season, Some(2));
            assert_eq!(parsed.kind, "tv");
        }
        assert_eq!(parse_media("Step2 (2024).mkv").title, "Step2");
    }

    #[test]
    fn season_parent_overrides_romanized_episode_filename() {
        let parsed = import_media_name(
            "Mushoku.Tensei.Jobless.Reincarnation.2021.S01E01.2160p.AI.Upscaled.H265.mkv",
            Path::new("无职转生 ～到了异世界就拿出真本事～ (2021)/Season 1/Mushoku.Tensei.Jobless.Reincarnation.2021.S01E01.2160p.AI.Upscaled.H265.mkv"),
        );
        assert_eq!(parsed.title, "无职转生 ～到了异世界就拿出真本事～");
        assert_eq!(parsed.year, "2021");
        assert_eq!(parsed.season, Some(1));
        assert_eq!(parsed.kind, "tv");
    }

    #[test]
    fn release_tags_are_removed_before_movie_search() {
        for (filename, expected) in [
            (
                "The.Wandering.Earth.Beyond.2020.Special.Edition.2020.WEB-DL.2160p.HEVC.Hybrid.DV.P8.HDR.Vivid.60fps.DDP.5.1.Atmos.mkv",
                "The Wandering Earth Beyond",
            ),
            (
                "Thor.The.Dark.World.2013.2160p.BluRay.REMUX.HEVC.DTS-HD.MA.TrueHD.7.1.Atmos.mkv",
                "Thor The Dark World",
            ),
        ] {
            assert_eq!(parse_media(filename).title, expected, "{filename}");
        }
    }

    #[test]
    fn imports_recognize_tmdb_ids_in_source_directory_names() {
        assert_eq!(
            tmdb_id_from_path(Path::new(
                "合集/流浪地球 (2019) {tmdb-535167}/The.Wandering.Earth.mkv",
            )),
            Some(535167)
        );
        assert_eq!(
            tmdb_id_from_path(Path::new("标题 {tmdbid-81502}/Season 1/01.mkv")),
            Some(81502)
        );
    }
}

#[cfg(test)]
mod unclassified_layout_regressions {
    use super::*;
    #[test]
    fn incomplete_metadata_and_root_files_do_not_add_artificial_directories() {
        for path in ["01.mkv", "作品/第二季/01.mkv"] {
            let relative = Path::new(path);
            let filename = relative.file_name().unwrap().to_str().unwrap();
            let media = import_media_name(filename, relative);
            let incomplete = json!({"id":1,"title":"候选作品","mediaType":"tv"});
            assert_eq!(
                library_destination_dir(Path::new("Media"), &media, Some(&incomplete), "待确认"),
                Path::new("Media/待确认").join(relative.parent().unwrap())
            );
            assert_eq!(
                library_file_stem(&media, Some(&incomplete), "", filename),
                "01"
            );
        }
    }
}

#[cfg(test)]
mod generation_and_rename_regressions {
    use super::*;
    #[test]
    fn recognizes_anime_groups_episode_styles_and_literal_movie_titles() {
        for (name,title,episode) in [
            ("[ANi] 葬送的芙莉莲 - 01 [1080P][Baha].mkv","葬送的芙莉莲",Some(1)),
            ("[字幕组][凡人修仙传][196][2160p].mkv","凡人修仙传",Some(196)),
            ("凡人修仙传.1x196.1080p.mkv","凡人修仙传",Some(196)),
            ("凡人修仙传 第一百九十六话.mkv","凡人修仙传",Some(196)),
            ("第五元素 (1997).mkv","第五元素",None),
            ("2001太空漫游 (1968).mkv","2001太空漫游",None),
        ] {
            let parsed=parse_media(name);assert_eq!(parsed.title,title,"{name}");assert_eq!(parsed.episode,episode,"{name}");
        }
        let parsed=import_media_name("凡人修仙传.S02E01.mkv",Path::new("动漫合集 第三季/凡人修仙传/第二季/凡人修仙传.S02E01.mkv"));
        assert_eq!(parsed.title,"凡人修仙传");assert_eq!(parsed.season,Some(2));
    }
    #[test]
    fn strm_configuration_is_backward_compatible_validated_and_normalized() {
        let legacy:PersistedState=serde_json::from_value(json!({"connections":[],"tmdb":null})).unwrap();
        assert_eq!(legacy.strm.metadata_extensions,"ass,srt,ssa,sub,vtt");
        let settings=validated_strm_settings(&json!({"videoExtensions":"MKV,mp4，mkv","minSizeMb":0,"metadataExtensions":"srt,ass"})).unwrap();
        assert_eq!(settings.video_extensions,"mkv,mp4");assert_eq!(settings.min_size_mb,0);
        assert!(has_extension("test.MKV",&settings.video_extensions));
        for bad in [json!({"videoExtensions":".mkv"}),json!({"videoExtensions":"mkv","metadataExtensions":"mkv"}),json!({"metadataExtensions":"strm"}),json!({"minSizeMb":10241})] {assert!(validated_strm_settings(&bad).is_err());}
    }
    #[test]
    fn subtitle_names_follow_video_names_and_keep_language_suffixes() {
        let destinations=HashMap::from([(PathBuf::from("source/第一集.mkv"),PathBuf::from("out/动漫/作品/Season 1/作品.S01E01.strm"))]);
        assert_eq!(metadata_destination(Path::new("source/第一集.zh-CN.ass"),Path::new("out"),"未分类",&destinations),PathBuf::from("out/动漫/作品/Season 1/作品.S01E01.zh-CN.ass"));
        assert_eq!(metadata_destination(Path::new("source/poster.jpg"),Path::new("out"),"未分类",&destinations),PathBuf::from("out/动漫/作品/Season 1/poster.jpg"));
    }
    #[test]
    fn rename_preflight_detects_collisions_and_preserves_folder_dots() {
        let root=std::env::temp_dir().join(format!("moviettd-rename-{}",uuid::Uuid::new_v4()));fs::create_dir_all(root.join("Show.S01")).unwrap();
        fs::write(root.join("EP01.strm"),"url").unwrap();fs::write(root.join("EP02.strm"),"url").unwrap();
        let paths=vec![root.join("EP01.strm"),root.join("EP02.strm")];
        let preview=rename_preview(&paths,&json!({"mode":"template","name":"第{n}集","start":5,"padding":3})).unwrap();
        assert!(preview.valid);assert_eq!(preview.items[0].new_name,"第005集.strm");
        let conflict=rename_preview(&paths,&json!({"mode":"replace","find":"EP0[12]","replace":"Episode","regex":true})).unwrap();assert!(!conflict.valid);
        let folder=rename_preview(&[root.join("Show.S01")],&json!({"mode":"affix","prefix":"新"})).unwrap();assert_eq!(folder.items[0].new_name,"新Show.S01");
        let unsafe_name=rename_preview(&paths[..1],&json!({"mode":"single","name":"../escape"})).unwrap();assert!(!unsafe_name.valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod share_presentation_regressions {
    use super::*;
    fn entry(name:&str,is_dir:bool)->RemoteFile {RemoteFile {id:"1".into(),name:name.into(),is_dir,size:0,s3:String::new(),etag:String::new()}}
    #[test]
    fn share_names_prefer_root_folders_and_support_direct_files() {
        assert_eq!(share_source_names(&[entry("说明.txt",false),entry("第一部",true),entry("第二部",true)]),vec!["第一部","第二部"]);
        assert_eq!(share_source_names(&[entry("电影.mkv",false)]),vec!["电影.mkv"]);
        assert!(share_source_names(&[]).is_empty());
    }
}
