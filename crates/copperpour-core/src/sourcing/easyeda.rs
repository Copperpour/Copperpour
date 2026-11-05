//! The EasyEDA component API (#209): downloads a part's symbol and footprint
//! bundle by LCSC number, with a local disk cache. `import_jlcpcb_part`
//! (`tools/easyeda.rs`) converts the bundle to KiCad.

use std::path::PathBuf;

/// Resolve the local disk cache directory for EasyEDA API responses.
/// `None` when the user's home directory is unknown; the cache is then skipped.
pub(crate) fn easyeda_cache_dir() -> Option<PathBuf> {
    crate::app_dirs::user_data_dir().map(|dir| dir.join("cache").join("easyeda"))
}

/// Fetch an EasyEDA component bundle by LCSC ID, using local disk cache first
/// and falling back across easyeda.com and lceda.cn with standard browser headers.
pub(crate) async fn fetch_easyeda_bundle(lcsc_id: &str) -> anyhow::Result<serde_json::Value> {
    let clean_id = lcsc_id.trim();
    if clean_id.is_empty() {
        anyhow::bail!("LCSC part number cannot be empty");
    }

    let cache_dir = easyeda_cache_dir();
    let cache_file = cache_dir
        .as_ref()
        .map(|dir| dir.join(format!("{clean_id}.json")));
    if let Some(cache_file) = cache_file.as_ref().filter(|file| file.is_file()) {
        if let Ok(content) = std::fs::read_to_string(cache_file) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if json.get("dataStr").is_some() {
                    return Ok(json);
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let primary_url =
        format!("https://easyeda.com/api/products/{clean_id}/components?version=6.4.19.5");
    let fallback_url =
        format!("https://lceda.cn/api/products/{clean_id}/components?version=6.4.19.5");

    let mut body = None;
    for url in [&primary_url, &fallback_url] {
        if let Ok(resp) = client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if json["success"].as_bool() == Some(true) && json.get("result").is_some() {
                        body = Some(json["result"].clone());
                        break;
                    }
                }
            }
        }
    }

    let result = body.ok_or_else(|| {
        anyhow::anyhow!("Part '{clean_id}' not found in EasyEDA or could not be downloaded")
    })?;

    if let (Some(cache_dir), Some(cache_file)) = (cache_dir, cache_file) {
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            tracing::warn!("Failed to create EasyEDA cache dir: {e}");
        } else if let Ok(s) = serde_json::to_string(&result) {
            let _ = std::fs::write(&cache_file, s);
        }
    }

    Ok(result)
}
