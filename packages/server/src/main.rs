//! Native authoritative PWMTF process entry point.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use tokio::sync::watch;

use pwmtf_server::{
    CANONICAL_ORIGIN, GOOGLE_ISSUER, GoogleOidcClient, HttpState, http_router, migrate,
};
use thiserror::Error;

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const DEFAULT_DATABASE_PATH: &str = "/data/pwmtf.db";
const DEFAULT_CALLBACK: &str = "https://pwmtf.hyperchad.dev/auth/google/callback";
const AUTH_RECORD_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("server startup failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), StartupError> {
    let configuration = Configuration::from_environment()?;
    validate_web_bundle_identity(
        &configuration.web_root,
        configuration.expected_build_id.as_deref(),
        configuration.expected_source_hash.as_deref(),
    )?;
    let backend: Arc<dyn switchy_database::Database> = switchy_database_connection::builder()
        .turso()
        .with_path(&configuration.database_path)
        .with_busy_timeout(Duration::from_secs(5))
        .build()
        .await?
        .into();
    let db: Arc<dyn switchy_database::Database> =
        Arc::new(pwmtf_server::SerializedDatabase::new(backend));
    migrate(&*db).await?;
    let state = if configuration.development_mode {
        #[cfg(feature = "insecure")]
        {
            HttpState::development(db, &configuration.public_origin)
                .with_web_root(configuration.web_root)
        }
        #[cfg(not(feature = "insecure"))]
        unreachable!("development mode requires the insecure feature")
    } else {
        let client_id = configuration
            .google_client_id
            .as_deref()
            .ok_or(StartupError::Configuration)?;
        let client_secret = configuration
            .google_client_secret
            .as_deref()
            .ok_or(StartupError::Configuration)?;
        let oidc = Arc::new(
            GoogleOidcClient::discover(client_id, client_secret, &configuration.google_callback)
                .await?,
        );
        HttpState::production(db, oidc).with_web_root(configuration.web_root)
    };
    let state = Arc::new(state);
    state.reset_lobby_presence().await?;
    state.recover_all_matches().await?;
    let (scheduler_shutdown, scheduler_signal) = watch::channel(false);
    let scheduler = tokio::spawn(scheduler_loop(Arc::clone(&state), scheduler_signal));
    let listener = tokio::net::TcpListener::bind(configuration.bind).await?;
    axum::serve(listener, http_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    scheduler_shutdown
        .send(true)
        .map_err(|_| StartupError::Scheduler)?;
    tokio::time::timeout(Duration::from_secs(5), scheduler)
        .await
        .map_err(|_| StartupError::Scheduler)?
        .map_err(|_| StartupError::Scheduler)?;
    Ok(())
}

async fn scheduler_loop(state: Arc<HttpState>, mut shutdown: watch::Receiver<bool>) {
    let mut next_auth_cleanup = 0_u64;
    loop {
        let now = unix_millis();
        if now >= next_auth_cleanup {
            if let Err(error) = state.cleanup_expired_auth_records(now).await {
                eprintln!("expired authentication record cleanup failed: {error}");
            }
            next_auth_cleanup = now.saturating_add(
                u64::try_from(AUTH_RECORD_CLEANUP_INTERVAL.as_millis()).unwrap_or(u64::MAX),
            );
        }
        if let Err(error) = state
            .poll_due_deadlines(pwmtf_server::DeadlineMillis::new(now))
            .await
        {
            state.mark_scheduler_unhealthy();
            eprintln!("authoritative deadline polling failed: {error}");
        }
        let sleep = state
            .next_deadline()
            .await
            .map_or(Duration::from_secs(1), |deadline| {
                Duration::from_millis(deadline.due_at.value().saturating_sub(unix_millis()).max(1))
            })
            .min(Duration::from_secs(1));
        if !wait_for_scheduler_tick(&mut shutdown, sleep).await {
            break;
        }
    }
}

async fn wait_for_scheduler_tick(shutdown: &mut watch::Receiver<bool>, sleep: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(sleep) => true,
        result = shutdown.changed() => result.is_ok() && !*shutdown.borrow(),
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

async fn shutdown_signal() {
    let control_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("failed to install Ctrl-C handler: {error}");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                eprintln!("failed to install termination handler: {error}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
}

struct Configuration {
    bind: SocketAddr,
    database_path: PathBuf,
    web_root: PathBuf,
    google_client_id: Option<String>,
    google_client_secret: Option<String>,
    google_callback: String,
    #[cfg(feature = "insecure")]
    public_origin: String,
    development_mode: bool,
    expected_build_id: Option<String>,
    expected_source_hash: Option<String>,
}

impl Configuration {
    fn from_environment() -> Result<Self, StartupError> {
        let development_mode = development_mode()?;
        let canonical_origin =
            environment("PWMTF_CANONICAL_ORIGIN").unwrap_or_else(|| CANONICAL_ORIGIN.to_owned());
        if development_mode {
            validate_development_origin(&canonical_origin)?;
        } else if canonical_origin != CANONICAL_ORIGIN {
            return Err(StartupError::Configuration);
        }
        let google_issuer =
            environment("PWMTF_GOOGLE_ISSUER").unwrap_or_else(|| GOOGLE_ISSUER.to_owned());
        if google_issuer != GOOGLE_ISSUER {
            return Err(StartupError::Configuration);
        }
        let callback = environment("PWMTF_GOOGLE_CALLBACK").unwrap_or_else(|| {
            if development_mode {
                format!("{canonical_origin}/auth/google/callback")
            } else {
                DEFAULT_CALLBACK.to_owned()
            }
        });
        if !development_mode && callback != DEFAULT_CALLBACK {
            return Err(StartupError::Configuration);
        }
        let (google_client_id, google_client_secret) = google_configuration(development_mode)?;
        Ok(Self {
            bind: environment("PWMTF_BIND")
                .unwrap_or_else(|| DEFAULT_BIND.to_owned())
                .parse()
                .map_err(|_| StartupError::Configuration)?,
            database_path: environment("PWMTF_DATABASE_PATH")
                .unwrap_or_else(|| DEFAULT_DATABASE_PATH.to_owned())
                .into(),
            web_root: environment("PWMTF_WEB_ROOT")
                .unwrap_or_else(|| "/app/dist".to_owned())
                .into(),
            google_client_id,
            google_client_secret,
            google_callback: callback,
            #[cfg(feature = "insecure")]
            public_origin: canonical_origin,
            development_mode,
            expected_build_id: optional_identity_environment("PWMTF_EXPECTED_BUILD_ID")?
                .or_else(|| deployment_identity_file("/app/pwmtf-build-id")),
            expected_source_hash: optional_hash_environment("PWMTF_EXPECTED_SOURCE_HASH")?
                .or_else(|| deployment_identity_file("/app/pwmtf-source-hash")),
        })
    }
}

fn development_mode() -> Result<bool, StartupError> {
    let enabled = environment("PWMTF_DEV_MODE").is_some_and(|value| value == "true");
    validate_development_mode(enabled, cfg!(feature = "insecure"))
}

const fn validate_development_mode(
    enabled: bool,
    insecure_feature: bool,
) -> Result<bool, StartupError> {
    if enabled != insecure_feature {
        return Err(StartupError::Configuration);
    }
    Ok(enabled)
}

fn validate_development_origin(origin: &str) -> Result<(), StartupError> {
    let Some(authority) = origin.strip_prefix("http://") else {
        return Err(StartupError::Configuration);
    };
    let host = authority.split(':').next().unwrap_or_default();
    if !matches!(host, "127.0.0.1" | "localhost") || authority.contains(['/', '?', '#', '@']) {
        return Err(StartupError::Configuration);
    }
    Ok(())
}

fn google_configuration(
    development_mode: bool,
) -> Result<(Option<String>, Option<String>), StartupError> {
    let client_id = environment("PWMTF_GOOGLE_CLIENT_ID");
    let client_secret = environment("PWMTF_GOOGLE_CLIENT_SECRET");
    match (client_id, client_secret) {
        (None, None) if development_mode => Ok((None, None)),
        (Some(client_id), Some(client_secret)) if !development_mode => Ok((
            Some(validate_secret_value(client_id)?),
            Some(validate_secret_value(client_secret)?),
        )),
        _ => Err(StartupError::Configuration),
    }
}

fn environment(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn deployment_identity_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn optional_identity_environment(name: &str) -> Result<Option<String>, StartupError> {
    let Some(value) = environment(name) else {
        return Ok(None);
    };
    if value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StartupError::Configuration);
    }
    Ok(Some(value))
}

fn optional_hash_environment(name: &str) -> Result<Option<String>, StartupError> {
    let Some(value) = environment(name) else {
        return Ok(None);
    };
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StartupError::Configuration);
    }
    Ok(Some(value))
}

fn validate_web_bundle_identity(
    web_root: &std::path::Path,
    expected_build_id: Option<&str>,
    expected_source_hash: Option<&str>,
) -> Result<(), StartupError> {
    let bootstrap = std::fs::read_to_string(web_root.join("bootstrap.js"))?;
    if let Some(expected) = expected_build_id
        && !bootstrap.contains(&format!("const candidateBuildId = \"{expected}\";"))
    {
        return Err(StartupError::BundleIdentity);
    }
    if let Some(expected) = expected_source_hash
        && !bootstrap.contains(&format!("const candidateSourceHash = \"{expected}\";"))
    {
        return Err(StartupError::BundleIdentity);
    }
    Ok(())
}

fn validate_secret_value(value: String) -> Result<String, StartupError> {
    if value.len() > 4_096
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        Err(StartupError::Configuration)
    } else {
        Ok(value)
    }
}

#[derive(Debug, Error)]
enum StartupError {
    #[error("server configuration is invalid")]
    Configuration,
    #[error("web bundle identity does not match the native deployment")]
    BundleIdentity,
    #[error("database initialization failed")]
    Database(#[from] switchy_database_connection::InitTursoError),
    #[error("database migration failed")]
    Migration(#[from] switchy_schema::MigrationError),
    #[error("Google OIDC initialization failed")]
    Oidc(#[from] pwmtf_server::GoogleOidcError),
    #[error("transport recovery failed")]
    Transport(#[from] pwmtf_server::TransportError),
    #[error("authoritative scheduler did not stop cleanly")]
    Scheduler,
    #[error("server I/O failed")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_configuration_rejects_whitespace_controls_and_oversize_values() {
        assert_eq!(
            validate_secret_value("client-id".to_owned()).unwrap(),
            "client-id"
        );
        for value in [" secret", "secret ", "sec\nret", "two words"] {
            assert!(matches!(
                validate_secret_value(value.to_owned()),
                Err(StartupError::Configuration)
            ));
        }
        assert!(matches!(
            validate_secret_value("x".repeat(4_097)),
            Err(StartupError::Configuration)
        ));
    }

    #[test]
    fn web_bundle_identity_is_verified_when_pinned() {
        let root =
            std::env::temp_dir().join(format!("pwmtf-bundle-identity-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("bootstrap.js"),
            "const candidateBuildId = \"build-1\";\nconst candidateSourceHash = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";\n",
        )
        .unwrap();
        assert!(
            validate_web_bundle_identity(
                &root,
                Some("build-1"),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            )
            .is_ok()
        );
        assert!(validate_web_bundle_identity(&root, Some("build-2"), None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_wait_stops_cooperatively_on_shutdown() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (sender, mut receiver) = watch::channel(false);
            let wait = tokio::spawn(async move {
                wait_for_scheduler_tick(&mut receiver, Duration::from_secs(60)).await
            });
            tokio::task::yield_now().await;
            sender.send(true).unwrap();
            assert!(
                !tokio::time::timeout(Duration::from_secs(1), wait)
                    .await
                    .unwrap()
                    .unwrap()
            );
        });
    }

    #[test]
    fn development_mode_and_insecure_feature_must_agree() {
        assert!(!validate_development_mode(false, false).unwrap());
        assert!(validate_development_mode(true, true).unwrap());
        assert!(validate_development_mode(true, false).is_err());
        assert!(validate_development_mode(false, true).is_err());
    }

    #[cfg(feature = "insecure")]
    #[test]
    fn development_configuration_is_loopback_and_excludes_google() {
        assert!(validate_development_origin("http://127.0.0.1:8080").is_ok());
        assert!(validate_development_origin("http://localhost:8080").is_ok());
        for origin in [
            "https://127.0.0.1:8080",
            "http://0.0.0.0:8080",
            "http://evil.example:8080",
            "http://localhost:8080/path",
        ] {
            assert!(validate_development_origin(origin).is_err());
        }
    }

    #[test]
    fn production_constants_use_canonical_origin() {
        assert_eq!(CANONICAL_ORIGIN, "https://pwmtf.hyperchad.dev");
        assert_eq!(
            DEFAULT_CALLBACK,
            format!("{CANONICAL_ORIGIN}/auth/google/callback")
        );
        assert_eq!(GOOGLE_ISSUER, "https://accounts.google.com");
    }
}
