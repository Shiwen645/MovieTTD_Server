// Capacity endpoints follow YunX's Pan123Api, QuarkApi and UCApi.
fn parse_quota(provider: &str, value: &Value) -> Option<Value> {
    let data = &value["data"];
    let number = |key: &str| {
        data[key]
            .as_u64()
            .or_else(|| data[key].as_str()?.parse().ok())
    };
    let (used, total) = if provider == "pan123" {
        (
            number("SpaceUsed")?,
            number("SpacePermanent")?.checked_add(number("SpaceTemp").unwrap_or(0))?,
        )
    } else {
        (number("use_capacity")?, number("total_capacity")?)
    };
    if total == 0 {
        return None;
    }
    Some(json!({"used":used,"total":total,"updatedAt":now()}))
}
fn drive_quota(provider: &str) -> Result<Value, String> {
    let credential = secret(&format!("drive-{provider}")).ok_or("尚未绑定账号")?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|_| "容量服务初始化失败")?;
    let request = if provider == "pan123" {
        pan123_validation_request(&client, &credential, &pan123_device_id()?)
    } else {
        let (variable, url, origin) = match provider {
            "quark" => (
                "MOVIETTD_QUARK_QUOTA_URL",
                "https://drive-pc.quark.cn/1/clouddrive/member?pr=ucpro&fr=pc&fetch_subscribe=true&_ch=home",
                "https://pan.quark.cn",
            ),
            "uc" => (
                "MOVIETTD_UC_QUOTA_URL",
                "https://pc-api.uc.cn/1/clouddrive/member?pr=UCBrowser&fr=pc&fetch_subscribe=true&_ch=home",
                "https://drive.uc.cn",
            ),
            _ => return Err("不支持该网盘".into()),
        };
        client.get(provider_url(variable,url)).header("Cookie",credential).header("User-Agent",if provider=="uc" {"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) uc-cloud-drive/1.6.1 Chrome/100.0.4896.160 Electron/18.3.5 Safari/537.36"} else {UA}).header("Origin",origin).header("Referer",format!("{origin}/"))
    };
    let value = request
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<Value>())
        .map_err(|_| "容量接口暂时不可用，请稍后重试")?;
    parse_quota(provider, &value).ok_or_else(|| "网盘未返回有效容量，登录可能已过期".into())
}
