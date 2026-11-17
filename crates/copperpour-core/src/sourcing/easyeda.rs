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

/// Downloaded 3D model payloads for an EasyEDA `outline3D` UUID.
pub(crate) struct Easyeda3dAssets {
    pub step: Result<Vec<u8>, String>,
    pub obj: Result<String, String>,
}

fn easyeda_http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Copperpour/1.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()?)
}

/// Download STEP and OBJ payloads for an EasyEDA 3D model UUID (`SVGNODE.attrs.uuid`).
pub(crate) async fn fetch_easyeda_3d_assets(uuid: &str) -> anyhow::Result<Easyeda3dAssets> {
    let clean_uuid = uuid.trim();
    if clean_uuid.is_empty() {
        anyhow::bail!("EasyEDA 3D model UUID cannot be empty");
    }
    let step_url = format!("https://modules.easyeda.com/qAxj6KHrDKw4blvCG8QJPs7Y/{clean_uuid}");
    let obj_url = format!("https://modules.easyeda.com/3dmodel/{clean_uuid}");

    fetch_easyeda_3d_assets_at(&step_url, &obj_url).await
}

async fn fetch_easyeda_3d_assets_at(
    step_url: &str,
    obj_url: &str,
) -> anyhow::Result<Easyeda3dAssets> {
    let client = easyeda_http_client()?;
    let (step_res, obj_res) = tokio::join!(client.get(step_url).send(), client.get(obj_url).send());

    let step = match step_res {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(e) => Err(format!("reading STEP body failed: {e}")),
        },
        Ok(resp) => Err(format!("STEP endpoint returned HTTP {}", resp.status())),
        Err(e) => Err(format!("STEP request failed: {e}")),
    };

    let obj = match obj_res {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(text) => Ok(text),
            Err(e) => Err(format!("reading OBJ body failed: {e}")),
        },
        Ok(resp) => Err(format!("OBJ endpoint returned HTTP {}", resp.status())),
        Err(e) => Err(format!("OBJ request failed: {e}")),
    };

    Ok(Easyeda3dAssets { step, obj })
}

#[cfg(test)]
mod model_download_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn model_download_preserves_payloads_and_reports_endpoint_failure() {
        let obj = include_str!("../../tests/fixtures/easyeda-3d/placement.obj");
        let step = include_bytes!("../../tests/fixtures/easyeda-3d/resistor.step");
        for step_status in [200, 404] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let mut handlers = Vec::new();
                for _ in 0..2 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    handlers.push(tokio::spawn(async move {
                        let mut request = [0u8; 4096];
                        let n = socket.read(&mut request).await.unwrap();
                        let is_step = request[..n].starts_with(b"GET /step ");
                        let status = if is_step { step_status } else { 200 };
                        let body: &[u8] = if is_step && status == 200 {
                            step
                        } else if is_step {
                            b"missing"
                        } else {
                            obj.as_bytes()
                        };
                        let header = format!(
                        "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                        socket.write_all(header.as_bytes()).await.unwrap();
                        socket.write_all(body).await.unwrap();
                        socket.shutdown().await.unwrap();
                    }));
                }
                for handler in handlers {
                    handler.await.unwrap();
                }
            });
            let assets =
                fetch_easyeda_3d_assets_at(&format!("{base}/step"), &format!("{base}/obj"))
                    .await
                    .unwrap();
            assert_eq!(assets.obj.unwrap(), obj);
            if step_status == 200 {
                assert_eq!(assets.step.unwrap(), step);
            } else {
                assert!(assets.step.unwrap_err().contains("404"));
            }
            server.await.unwrap();
        }
    }
}
