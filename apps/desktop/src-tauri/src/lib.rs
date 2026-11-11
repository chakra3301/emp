mod daemon;

/// Daemon discovery for the window: `{ wsUrl, token }`, starting empd via launchd if needed.
/// Runs off the main thread (it may wait for the daemon to come up).
#[tauri::command]
async fn emp_connect() -> Result<daemon::ConnectInfo, String> {
    tauri::async_runtime::spawn_blocking(daemon::connect).await.map_err(|e| e.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![emp_connect])
        .setup(|_app| {
            // Keep the login item pointing at this installed copy; never blocks the window.
            std::thread::spawn(|| {
                if let Err(e) = daemon::install_login_item() {
                    eprintln!("EMP: login item: {e}");
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running EMP");
}
