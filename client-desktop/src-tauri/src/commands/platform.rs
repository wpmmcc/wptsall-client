use serde::Serialize;

#[derive(Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub version: String,
    pub data_dir: String,
}

#[tauri::command]
pub async fn get_platform_info() -> Result<PlatformInfo, String> {
    let dirs = directories::ProjectDirs::from("cc", "wpmm", "wptsall-client")
        .ok_or("Cannot determine data directory")?;

    Ok(PlatformInfo {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir: dirs.data_dir().display().to_string(),
    })
}
