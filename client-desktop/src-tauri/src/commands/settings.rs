use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ClientSettings {
    pub auto_start: bool,
    pub language: String,
    pub log_level: String,
    pub proxy: Option<String>,
}

#[tauri::command]
pub async fn get_settings() -> Result<ClientSettings, String> {
    Ok(ClientSettings {
        auto_start: false,
        language: "en".into(),
        log_level: "info".into(),
        proxy: None,
    })
}

#[tauri::command]
pub async fn update_settings(settings: ClientSettings) -> Result<(), String> {
    let _ = settings;
    Ok(())
}
