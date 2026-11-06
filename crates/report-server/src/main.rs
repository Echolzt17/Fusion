use fusion_report_server::{now, router, Server, Store};
use std::{path::PathBuf, io::Write};
fn paths() -> (PathBuf, PathBuf) {
    (std::env::var_os("FUSION_REPORT_DATA").map(PathBuf::from).unwrap_or_else(|| "/var/lib/fusion-reports".into()),
     std::env::var_os("FUSION_REPORT_ADMIN_FILE").map(PathBuf::from).unwrap_or_else(|| "/etc/fusion-reports/admin.key".into()))
}
#[tokio::main]
async fn main() {
    if let Err(code) = run().await { eprintln!("fusion-report-server: {code}"); std::process::exit(1); }
}
async fn run() -> Result<(), &'static str> {
    let (root, key_file) = paths();
    let action = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    if action == "init-key" {
        if let Some(p) = key_file.parent() { std::fs::create_dir_all(p).map_err(|_| "key_directory_failed")?; }
        let mut options = std::fs::OpenOptions::new(); options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(key_file).map_err(|_| "key_exists_or_unwritable")?;
        let key = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        file.write_all(key.as_bytes()).and_then(|_| file.sync_all()).map_err(|_| "key_write_failed")?;
        println!("admin_key_created"); return Ok(());
    }
    let max_bytes = std::env::var("FUSION_REPORT_MAX_BYTES").ok().and_then(|s| s.parse().ok()).unwrap_or(2 * 1024 * 1024 * 1024);
    let mut store = Store::open(&root, max_bytes)?;
    match action.as_str() {
        "backup" => { store.backup(now())?; println!("backup_verified"); return Ok(()); }
        "sweep" => { store.sweep(now())?; println!("sweep_complete"); return Ok(()); }
        "serve" => {}
        _ => return Err("unknown_action"),
    }
    let key = std::fs::read_to_string(key_file).map_err(|_| "admin_key_unavailable")?;
    let server = Server::new(store, key.trim().into(), 10 * 1024 * 1024 * 1024)?;
    let worker = server.clone();
    tokio::spawn(async move {
        loop {
            let s = worker.clone();
            if !matches!(tokio::task::spawn_blocking(move || s.store.lock().map_err(|_| "database_lock_failed")?.sweep(now())).await, Ok(Ok(()))) {
                eprintln!("fusion-report-server: sweep_failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs((900 - now().rem_euclid(900)) as u64)).await;
        }
    });
    let port: u16 = std::env::var("FUSION_REPORT_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(18880);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await.map_err(|_| "listen_failed")?;
    println!("fusion-report-server: ready_loopback");
    axum::serve(listener, router(server)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await.map_err(|_| "serve_failed")
}
