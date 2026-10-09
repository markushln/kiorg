use std::sync::Arc;

use tokio::signal::unix::{signal, SignalKind};
use tracing_subscriber::EnvFilter;
use xdg_desktop_portal_kiorg::{picker::Picker, portal::FileChooser};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.kiorg";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

#[tokio::main]
async fn main() -> zbus::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let picker = Arc::new(Picker::from_env());
    let _conn = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, FileChooser::new(picker))?
        .build()
        .await?;
    tracing::info!("{BUS_NAME} ready");

    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}
