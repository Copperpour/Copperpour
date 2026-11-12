//! Development entry point for the same browser service used by the MCP tool.
use copperpour_viewer::browser::BrowserViewer;
use std::{path::PathBuf, time::Duration};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: browser_viewer <root.kicad_sch> [kicad-cli]")?,
    );
    let cli = args
        .next()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| copperpour_viewer::resolve_kicad_cli(None));
    let viewer = BrowserViewer::start(&root, &cli, Duration::from_secs(30))
        .await
        .map_err(std::io::Error::other)?;
    println!("{}", viewer.url);
    tokio::signal::ctrl_c().await?;
    viewer.shutdown().await;
    Ok(())
}
