//! `root-mcp` — root's gigapipe reads, over the Model Context Protocol.
//!
//! An MCP client spawns this and talks JSON-RPC to it on stdio. It needs no
//! running copy of root, no port and no token; see [`inframonitoring_lib::mcp`]
//! for why that shape was chosen and where connections come from.
//!
//! Configure a client with:
//!
//! ```json
//! { "command": "/path/to/root-mcp" }
//! ```
//!
//! and either set `GIGAPIPE_HOST` in its environment or configure a gigapipe
//! connection in root itself.

use inframonitoring_lib::mcp;

fn main() -> std::process::ExitCode {
    // stdout is the protocol channel and must carry nothing but JSON-RPC, so
    // every diagnostic — including the tracing subscriber's own output — is
    // pointed at stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("root-mcp: could not start a runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let server = mcp::Server::from_environment_or_config(config_dir());

    match runtime.block_on(mcp::serve_stdio(server)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // A client that goes away closes the pipe, and that is an ordinary
        // end rather than a failure worth an exit code.
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("root-mcp: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Where root keeps `config.json`, so this serves the connections the app
/// already has.
///
/// `ROOT_CONFIG_DIR` overrides it, which is what the tests use and what a
/// second installation would need.
fn config_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("ROOT_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }

    // The same path the app itself resolves: the platform config directory
    // plus root's bundle identifier.
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".config"))
            })
    };

    base.map(|base| base.join("com.root.observability"))
}
