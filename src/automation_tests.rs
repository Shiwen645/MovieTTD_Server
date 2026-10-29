#[cfg(test)]
mod automation_regressions {
    use super::*;
    fn state() -> Arc<ServerState> {
        let dir =
            std::env::temp_dir().join(format!("moviettd-automation-{}", uuid::Uuid::new_v4()));
        load_state(dir.clone(), dir.join("media")).unwrap()
    }
    fn video(size: u64) -> RemoteFile {
        RemoteFile {
            id: "123".into(),
            name: "movie.mkv".into(),
            is_dir: false,
            size,
            s3: "s3".into(),
            etag: "etag".into(),
        }
    }
    #[test]
    fn drive_pause_and_share_password_are_recovered_without_guessing() {
        let dir = std::env::temp_dir().join(format!("moviettd-pause-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pause.json");
        assert_eq!(restore_drive_pause(&path).0, 0);
        atomic_json(&path, &json!({"until":now()+600,"reason":"限流"})).unwrap();
        assert!(restore_drive_pause(&path).0 > now());
        fs::write(&path, "invalid").unwrap();
        assert!(restore_drive_pause(&path).0 > now());
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            extract_password("https://www.123pan.com/s/abc-key?pwd=Ab12", ""),
            "Ab12"
        );
        assert_eq!(
            extract_password("https://www.123pan.com/s/abc-key 提取码 abcd", ""),
            "abcd"
        );
        assert_eq!(
            extract_password("https://www.123pan.com/s/abc-key?pwd=abcd", "manual"),
            "manual"
        );
    }
    #[test]
    fn duplicates_use_movie_or_season_episode_identity_and_upstream_sizes() {
        let state = state();
        let root = state.library_root.clone();
        let dir = root.join("电影/电影 (2024) {tmdb-42}");
        fs::create_dir_all(&dir).unwrap();
        let media = parse_media("电影.2024.1080p.mkv");
        let meta = json!({"id":42,"mediaType":"movie"});
        let first = dir.join("电影.1080p.strm");
        let second = dir.join("电影.2160p.strm");
        let result = write_resource(
            &state,
            &root,
            &first,
            "http://proxy/a",
            &video(1000),
            &media,
            Some(&meta),
            "skip",
        )
        .unwrap();
        assert!(result.written);
        let result = write_resource(
            &state,
            &root,
            &second,
            "http://proxy/b",
            &video(2000),
            &media,
            Some(&meta),
            "skip",
        )
        .unwrap();
        assert!(!result.written && result.preserve_only);
        assert!(!second.exists());
        assert!(
            !write_resource(
                &state,
                &root,
                &second,
                "http://proxy/b",
                &video(999),
                &media,
                Some(&meta),
                "larger"
            )
            .unwrap()
            .written
        );
        assert!(
            write_resource(
                &state,
                &root,
                &second,
                "http://proxy/b",
                &video(2000),
                &media,
                Some(&meta),
                "larger"
            )
            .unwrap()
            .written
        );
        assert!(!first.exists());
        assert_eq!(
            fs::read_to_string(&second).unwrap().trim(),
            "http://proxy/b"
        );
        let same = write_resource(
            &state,
            &root,
            &second,
            "http://proxy/b",
            &video(2000),
            &media,
            Some(&meta),
            "keep",
        )
        .unwrap();
        assert!(!same.written && !same.preserve_only);
        let version = write_resource(
            &state,
            &root,
            &second,
            "http://proxy/c",
            &video(500),
            &media,
            Some(&meta),
            "keep",
        )
        .unwrap();
        assert!(version.written);
        assert_ne!(version.path, second);
        assert!(second.exists());
        assert!(
            !write_resource(
                &state,
                &root,
                &first,
                "http://proxy/d",
                &video(1500),
                &media,
                Some(&meta),
                "larger"
            )
            .unwrap()
            .written
        );
        let replaced = write_resource(
            &state,
            &root,
            &first,
            "http://proxy/d",
            &video(1),
            &media,
            Some(&meta),
            "replace",
        )
        .unwrap();
        assert!(replaced.written && !second.exists() && !version.path.exists());
        fs::write(&first, "http://personal/edited").unwrap();
        assert!(
            !write_resource(
                &state,
                &root,
                &second,
                "http://proxy/e",
                &video(10000),
                &media,
                Some(&meta),
                "replace"
            )
            .unwrap()
            .written
        );
        let legacy = dir.join("legacy.strm");
        fs::write(&legacy, "legacy").unwrap();
        assert!(
            !write_resource(
                &state,
                &root,
                &second,
                "http://proxy/e",
                &video(10000),
                &media,
                Some(&meta),
                "larger"
            )
            .unwrap()
            .written
        );
        let tv1 = parse_media("剧集.S01E01.mkv");
        let tv2 = parse_media("剧集.S02E01.mkv");
        let tv3 = parse_media("剧集.S01E02.mkv");
        let tvmeta = json!({"id":42,"mediaType":"tv"});
        assert_ne!(
            resource_identity(&tv1, Some(&tvmeta), &first),
            resource_identity(&tv2, Some(&tvmeta), &first)
        );
        assert_ne!(
            resource_identity(&tv1, Some(&tvmeta), &first),
            resource_identity(&tv3, Some(&tvmeta), &first)
        );
        assert_ne!(
            resource_identity(&tv1, None, &first),
            resource_identity(&tv1, None, &second)
        );
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn generated_manifest_keeps_versions_but_cleans_unchanged_own_outputs() {
        let state = state();
        let root = state.library_root.clone();
        let first = root.join("first.strm");
        let second = root.join("second.strm");
        fs::write(&first, "http://proxy/first\n").unwrap();
        fs::write(&second, "http://proxy/second\n").unwrap();
        reconcile_generated(
            &state,
            "share",
            &root,
            &[json!({"path":first,"link":"http://proxy/first"})],
            false,
        )
        .unwrap();
        reconcile_generated(
            &state,
            "share",
            &root,
            &[json!({"path":second,"link":"http://proxy/second"})],
            true,
        )
        .unwrap();
        assert!(first.exists() && second.exists());
        reconcile_generated(&state, "share", &root, &[], false).unwrap();
        assert!(!first.exists() && !second.exists());
        fs::write(&first, "manual").unwrap();
        reconcile_generated(
            &state,
            "share",
            &root,
            &[json!({"path":first,"link":"manual","preserveOnly":true})],
            false,
        )
        .unwrap();
        reconcile_generated(&state, "share", &root, &[], false).unwrap();
        assert!(first.exists());
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn telegram_extraction_and_filters_are_scoped_and_preserve_passwords() {
        let links = telegram_share_links(
            "电影 https://www.123pan.com/s/abc-def 提取码：aB12\n https://123865.com/s/new-key?pwd=xy98 https://evil.invalid/s/a?pwd=x",
        );
        assert_eq!(
            links,
            vec![
                ("abc-def".into(), "aB12".into()),
                ("new-key".into(), "xy98".into())
            ]
        );
        assert!(keyword_match("电影 2160p", "电影,剧集", "预告|广告"));
        assert!(!keyword_match("电影预告", "电影", "预告"));
        assert!(!keyword_match("综艺", "电影,剧集", ""));
    }
    #[test]
    fn telegram_bursts_edits_and_restart_use_persistent_dedup_and_cursor() {
        let state = state();
        let vault_dir = state.dir.join("test-vault");
        fs::create_dir_all(&vault_dir).unwrap();
        VAULT.get_or_init(|| Mutex::new(Vault::open(&vault_dir).unwrap()));
        {
            let mut d = state.automation.lock().unwrap();
            d.monitors.push(ChannelMonitor {
                id: "monitor".into(),
                reference: "@fixture".into(),
                channel_id: "-100123".into(),
                title: "Fixture".into(),
                output_root: state.library_root.to_string_lossy().into(),
                include: "".into(),
                exclude: "广告".into(),
                enabled: true,
                last_message_id: 100,
                catchup_cursor: None,
                next_check_at: 0,
                last_error: String::new(),
            });
        }
        for id in 101..=200 {
            receive_telegram_message(&state,&json!({"channelId":"-100123","messageId":id,"text":"https://www.123pan.com/s/same-key 提取码：abcd"}),None).unwrap();
        }
        {
            let mut d = state.automation.lock().unwrap();
            assert_eq!(d.queue.len(), 1);
            assert_eq!(d.monitors[0].last_message_id, 200);
            assert!(d.monitors[0].catchup_cursor.is_none());
            d.queue[0].status = "completed".into();
            save_automation(&state, &d).unwrap();
        }
        let repeated = json!({"channelId":"-100123","messageId":200,"text":"https://www.123pan.com/s/same-key 提取码：abcd"});
        receive_telegram_message(&state, &repeated, None).unwrap();
        assert_eq!(state.automation.lock().unwrap().queue.len(), 1);
        let edited = json!({"channelId":"-100123","messageId":200,"text":"更新第2集 https://www.123pan.com/s/same-key 提取码：abcd"});
        receive_telegram_message(&state, &edited, None).unwrap();
        receive_telegram_message(&state, &edited, None).unwrap();
        assert_eq!(state.automation.lock().unwrap().queue.len(), 2);
        {
            let mut d = state.automation.lock().unwrap();
            d.monitors[0].catchup_cursor = Some(150);
            save_automation(&state, &d).unwrap();
        }
        let restored = load_automation(&state.dir).unwrap();
        assert_eq!(restored.queue.len(), 2);
        assert_eq!(restored.monitors[0].catchup_cursor, Some(150));
        assert!(!restored.tg_authorized);
        let raw = fs::read_to_string(state.dir.join("automation.json")).unwrap();
        assert!(!raw.contains("abcd"));
        let mut filtered = edited;
        filtered["messageId"] = json!(201);
        filtered["text"] = json!("广告 https://www.123pan.com/s/new-key");
        receive_telegram_message(&state, &filtered, None).unwrap();
        assert_eq!(state.automation.lock().unwrap().queue.len(), 2);
        // The shared test vault lives until process exit, matching production's single vault.
        if !vault_dir.join("vault.json").exists() {
            fs::remove_dir_all(&state.dir).unwrap();
        }
    }
}
