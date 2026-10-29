#[cfg(test)]
mod api_tests {
    use super::*;
    fn state() -> Arc<ServerState> {
        let dir=std::env::temp_dir().join(format!("moviettd-api-test-{}",uuid::Uuid::new_v4()));
        load_state(dir.clone(), dir.join("media")).unwrap()
    }
    #[test]
    fn vault_encrypts_at_rest_and_authenticates_secret_names() {
        let dir=std::env::temp_dir().join(format!("moviettd-vault-test-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let mut vault=Vault::open(&dir).unwrap();
        vault.set("drive-pan123","synthetic-secret-not-real").unwrap();
        let text=fs::read_to_string(dir.join("vault.json")).unwrap();
        assert!(!text.contains("synthetic-secret-not-real"));
        assert_eq!(vault.get("drive-pan123").as_deref(),Some("synthetic-secret-not-real"));
        vault.values.insert("another".into(),vault.values["drive-pan123"].clone());
        assert!(vault.get("another").is_none());
        vault.set("drive-pan123","").unwrap();
        assert!(vault.get("drive-pan123").is_none());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn management_requires_login_and_library_paths_are_confined() {
        let state=state();
        let response=dispatch(state.clone(),Method::GET,"/api/v1/tasks".into(),HeaderMap::new(),vec![]);
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
        assert!(safe_library_path(&state,"../").is_err());
        assert!(safe_library_path(&state,"/").is_err());
        let token="synthetic-admin-session";
        state.sessions.lock().unwrap().insert(token_hash(token),now()+60);
        let mut headers=HeaderMap::new();
        headers.insert(header::AUTHORIZATION,header::HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        let response=dispatch(state.clone(),Method::GET,"/api/v1/settings".into(),headers.clone(),vec![]);
        assert_eq!(response.status(),StatusCode::OK);
        state.sessions.lock().unwrap().insert(token_hash(token),now()-1);
        assert_eq!(dispatch(state.clone(),Method::GET,"/api/v1/settings".into(),headers,vec![]).status(),StatusCode::UNAUTHORIZED);
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn restart_preserves_history_and_marks_incomplete_tasks_interrupted() {
        let state=state();
        let task=Task{source_names:vec!["资源目录".into()],strm_settings:StrmSettings::default(),id:"test".into(),status:"running".into(),processed:1,total:2,message:"running".into(),error:None,created_at:now(),updated_at:now(),output_root:"".into(),min_size_mb:100,results:vec![]};
        state.jobs.lock().unwrap().push(task);
        persist_jobs(&state).unwrap();
        let restored=load_state(state.dir.clone(),state.library_root.clone()).unwrap();
        assert_eq!(restored.jobs.lock().unwrap()[0].status,"interrupted");
        assert_eq!(restored.jobs.lock().unwrap()[0].processed,1);
        assert_eq!(restored.jobs.lock().unwrap()[0].source_names,vec!["资源目录"]);
        let mut historical=serde_json::to_value(restored.jobs.lock().unwrap()[0].clone()).unwrap();
        historical.as_object_mut().unwrap().remove("sourceNames");
        assert!(serde_json::from_value::<Task>(historical).unwrap().source_names.is_empty());
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn storage_api_registers_roots_and_confines_file_operations() {
        let state=state();
        let root=state.dir.join("library-extra");
        fs::create_dir_all(&root).unwrap();
        let token="storage-test-session";
        state.sessions.lock().unwrap().insert(token_hash(token),now()+60);
        let mut headers=HeaderMap::new();
        headers.insert(header::AUTHORIZATION,header::HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        let invoke=|method:Method,target:String,body:Value|dispatch(state.clone(),method,target,headers.clone(),serde_json::to_vec(&body).unwrap());
        assert_eq!(invoke(Method::POST,"/api/v1/storage/roots".into(),json!({"path":root})).status(),StatusCode::OK);
        #[cfg(windows)]
        let system_root=PathBuf::from(format!("{}\\",std::env::var("SystemDrive").unwrap_or_else(|_|"C:".into())));
        #[cfg(not(windows))]
        let system_root=PathBuf::from("/");
        assert_eq!(invoke(Method::POST,"/api/v1/storage/roots".into(),json!({"path":system_root})).status(),StatusCode::BAD_REQUEST);
        let folder=invoke(Method::POST,"/api/v1/storage/files".into(),json!({"action":"mkdir","path":root,"name":"整理后"}));
        assert_eq!(folder.status(),StatusCode::OK);
        let directory=root.join("整理后");
        assert_eq!(invoke(Method::POST,"/api/v1/storage/files".into(),json!({"action":"create-file","path":directory,"name":"一集.strm"})).status(),StatusCode::OK);
        let file=directory.join("一集.strm");
        assert_eq!(invoke(Method::POST,"/api/v1/storage/files".into(),json!({"action":"write","path":directory,"filePath":file,"content":"http://example.invalid/media/1"})).status(),StatusCode::OK);
        assert_eq!(fs::read_to_string(&file).unwrap(),"http://example.invalid/media/1");
        let outside=state.dir.join("admin.json");
        assert_eq!(invoke(Method::POST,"/api/v1/storage/files".into(),json!({"action":"delete","path":directory,"paths":[outside]})).status(),StatusCode::BAD_REQUEST);
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn account_update_validates_password_persists_hash_and_revokes_other_sessions() {
        let state=state();
        let bootstrap:Value=serde_json::from_slice(&fs::read(state.dir.join("initial-admin.json")).unwrap()).unwrap();
        let current=bootstrap["password"].as_str().unwrap();
        let token="account-update-session";
        let mut headers=HeaderMap::new();
        headers.insert(header::AUTHORIZATION,header::HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        state.sessions.lock().unwrap().insert(token_hash(token),now()+60);
        state.sessions.lock().unwrap().insert(token_hash("other-session"),now()+60);
        let invoke=|body:Value|dispatch(state.clone(),Method::PUT,"/api/v1/auth/account".into(),headers.clone(),serde_json::to_vec(&body).unwrap());
        assert_eq!(invoke(json!({"username":"updated","currentPassword":"wrong","newPassword":"Aa2!Bb3@Cc4"})).status(),StatusCode::BAD_REQUEST);
        assert_eq!(invoke(json!({"username":"updated","currentPassword":current,"newPassword":"short"})).status(),StatusCode::BAD_REQUEST);
        assert_eq!(invoke(json!({"username":"bad name","currentPassword":current})).status(),StatusCode::BAD_REQUEST);
        assert_eq!(invoke(json!({"username":"updated","currentPassword":current,"newPassword":"Aa2!Bb3@Cc4"})).status(),StatusCode::OK);
        assert!(authorized(&state,&headers).is_some());
        assert_eq!(state.sessions.lock().unwrap().len(),1);
        assert!(!state.dir.join("initial-admin.json").exists());
        let restored=load_admin(&state.dir).unwrap();
        assert_eq!(restored.username,"updated");
        let hash=PasswordHash::new(&restored.password_hash).unwrap();
        assert!(Argon2::default().verify_password(b"Aa2!Bb3@Cc4",&hash).is_ok());
        assert!(Argon2::default().verify_password(current.as_bytes(),&hash).is_err());
        assert!(!fs::read_to_string(state.dir.join("admin.json")).unwrap().contains("Aa2!Bb3@Cc4"));
        assert_eq!(invoke(json!({"username":"renamed","currentPassword":"Aa2!Bb3@Cc4","newPassword":""})).status(),StatusCode::OK);
        assert_eq!(load_admin(&state.dir).unwrap().username,"renamed");
        fs::remove_dir_all(&state.dir).unwrap();
    }
    #[test]
    fn folder_size_deduplicates_nested_selection_and_counts_files() {
        let state=state();let parent=state.library_root.join("folder");let child=parent.join("nested");
        fs::create_dir_all(&child).unwrap();fs::write(parent.join("one.strm"),b"123").unwrap();fs::write(child.join("two.srt"),b"45678").unwrap();
        let result=storage_size(&[parent.clone(),child,parent.clone()]).unwrap();
        assert_eq!(result["size"],8);assert_eq!(result["fileCount"],2);assert_eq!(result["directoryCount"],2);
        assert_eq!(storage_file_kind(Path::new("sample.STRM")),"strm");
        assert_eq!(storage_file_kind(Path::new("sample.srt")),"subtitle");
        assert_eq!(storage_file_kind(Path::new("no-extension")),"other");
        assert!(storage_size(&[]).is_err());
        fs::remove_dir_all(&state.dir).unwrap();
    }

}
