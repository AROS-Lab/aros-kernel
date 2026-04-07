use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::sync::watch;

use aros_kernel::governor::{GovernorConfig, ResourceGovernor};
use aros_kernel::hardware::{pressure, probe};
use aros_kernel::store::SqliteStateStore;
use aros_kernel::supervisor::kernel::KernelSupervisor;
use aros_kernel::supervisor::process::{ProcessId, ProcessState, RestartPolicy};

#[derive(Parser)]
#[command(name = "aros-kernel", about = "AROS Kernel — Hardware-aware agent runtime engine")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the kernel daemon (default)
    Run {
        /// Directory for state persistence (SQLite, checkpoints)
        #[arg(long, default_value = "~/.aros/state")]
        state_dir: String,

        /// Health check interval in seconds
        #[arg(long, default_value_t = 5)]
        health_interval: u64,

        /// Log level filter (trace, debug, info, warn, error)
        #[arg(long, default_value = "info")]
        log_level: String,
    },

    /// Show recommended agent capacity based on hardware
    Recommend,

    /// Show current system status
    Status,
}

fn main() {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Run {
        state_dir: "~/.aros/state".into(),
        health_interval: 5,
        log_level: "info".into(),
    });

    match command {
        Commands::Run {
            state_dir,
            health_interval,
            log_level,
        } => {
            init_tracing(&log_level);

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");

            if let Err(e) = rt.block_on(run_daemon(state_dir, health_interval)) {
                tracing::error!("daemon exited with error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Recommend => {
            println!("Not yet implemented");
        }
        Commands::Status => {
            println!("Not yet implemented");
        }
    }
}

fn init_tracing(level: &str) {
    use tracing_subscriber::fmt;
    let filter = level.parse().unwrap_or(tracing::Level::INFO);
    fmt().with_max_level(filter).init();
    tracing::info!("aros-kernel v{}", env!("CARGO_PKG_VERSION"));
}

/// Resolve ~ to home directory and ensure the path exists.
fn resolve_state_dir(raw: &str) -> PathBuf {
    let expanded = if raw.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(&raw[2..])
        } else {
            PathBuf::from(raw)
        }
    } else {
        PathBuf::from(raw)
    };
    std::fs::create_dir_all(&expanded).expect("failed to create state directory");
    expanded
}

async fn run_daemon(
    state_dir: String,
    health_interval: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    // ── Signal handling ────────────────────────────────────────────
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(signal_handler(shutdown_tx));

    // ── Hardware probe ─────────────────────────────────────────────
    let hw = probe::probe_system();
    tracing::info!(
        cpus = hw.cpu_count,
        ram_total_mb = hw.ram_total_mb,
        ram_available_mb = hw.ram_available_mb,
        "hardware probed"
    );

    let initial_pressure = pressure::detect_pressure(&hw);
    tracing::info!(level = ?initial_pressure.level, "initial memory pressure");

    // ── State store ────────────────────────────────────────────────
    let state_path = resolve_state_dir(&state_dir);
    let db_path = state_path.join("aros.db");
    let _store = SqliteStateStore::open(
        db_path
            .to_str()
            .ok_or("invalid state directory path (non-UTF8)")?,
    )?;
    tracing::info!(path = %db_path.display(), "state store opened");

    // ── Supervisor ─────────────────────────────────────────────────
    let supervisor = Arc::new(KernelSupervisor::new());

    let critical_policy = RestartPolicy {
        max_restarts: 3,
        ..Default::default()
    };
    let loop_policy = RestartPolicy::default();
    let adapter_policy = RestartPolicy {
        max_restarts: 10,
        backoff_max: Duration::from_secs(30),
        ..Default::default()
    };

    supervisor
        .register(ProcessId::Init, critical_policy.clone())
        .await;
    supervisor
        .register(ProcessId::Kernel, critical_policy)
        .await;
    supervisor
        .register(ProcessId::Loop0Meta, loop_policy.clone())
        .await;
    supervisor
        .register(ProcessId::Loop1Agentic, loop_policy.clone())
        .await;
    supervisor
        .register(ProcessId::Loop2Harness, loop_policy)
        .await;
    supervisor
        .register(ProcessId::ModelAdapter, adapter_policy.clone())
        .await;
    supervisor
        .register(ProcessId::EmbeddingAdapter, adapter_policy)
        .await;

    tracing::info!("supervisor initialized, 7 processes registered");

    // ── Resource governor ──────────────────────────────────────────
    let governor = Arc::new(ResourceGovernor::new(GovernorConfig {
        system_rss_ceiling_mb: (hw.ram_total_mb as u32).saturating_sub(2048).max(4096),
        headroom_mb: 2048,
    }));
    governor.update_pressure(initial_pressure.level).await;
    tracing::info!("resource governor initialized");

    // ── Boot sequence ──────────────────────────────────────────────
    boot_sequence(&supervisor).await?;

    // ── Health check loop ──────────────────────────────────────────
    let health_handle = tokio::spawn(health_loop(
        supervisor.clone(),
        governor.clone(),
        health_interval,
        shutdown_rx.clone(),
    ));

    // ── Wait for shutdown signal ───────────────────────────────────
    tracing::info!("kernel running — waiting for shutdown signal");
    let mut wait_rx = shutdown_rx;
    let _ = wait_rx.changed().await;

    // ── Graceful shutdown ──────────────────────────────────────────
    tracing::info!("shutdown signal received");
    graceful_shutdown(&supervisor).await?;

    // Wait for health loop to exit
    let _ = health_handle.await;
    tracing::info!("aros-kernel stopped");

    Ok(())
}

async fn boot_sequence(
    supervisor: &KernelSupervisor,
) -> Result<(), Box<dyn std::error::Error>> {
    // Phase 1: Init
    supervisor
        .update_state(ProcessId::Init, ProcessState::Running)
        .await?;
    tracing::info!("init phase complete");

    // Phase 2: Kernel
    supervisor
        .update_state(ProcessId::Kernel, ProcessState::Running)
        .await?;
    tracing::info!("kernel process running");

    // Phase 3: Loops
    // TODO: spawn actual loop tasks (Loop 0 orchestrator, Loop 1 JSON-RPC, Loop 2 DAG executor)
    for pid in [
        ProcessId::Loop0Meta,
        ProcessId::Loop1Agentic,
        ProcessId::Loop2Harness,
    ] {
        supervisor
            .update_state(pid, ProcessState::Running)
            .await?;
        tracing::info!(?pid, "loop started");
    }

    // Phase 4: Adapters
    // TODO: spawn model adapter sidecar and embedding adapter
    for pid in [ProcessId::ModelAdapter, ProcessId::EmbeddingAdapter] {
        supervisor
            .update_state(pid, ProcessState::Running)
            .await?;
        tracing::info!(?pid, "adapter started");
    }

    let health = supervisor.health().await;
    tracing::info!(
        status = ?health.status,
        processes = health.active_processes.len(),
        "boot sequence complete"
    );

    Ok(())
}

async fn health_loop(
    supervisor: Arc<KernelSupervisor>,
    governor: Arc<ResourceGovernor>,
    interval_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    // Skip the immediate first tick
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Re-probe pressure
                let hw = probe::probe_system();
                let pressure_result = pressure::detect_pressure(&hw);
                governor.update_pressure(pressure_result.level).await;

                // Check health
                let health = supervisor.health().await;
                tracing::debug!(
                    status = ?health.status,
                    uptime_secs = health.kernel_uptime_secs,
                    processes = health.active_processes.len(),
                    "health tick"
                );

                if !supervisor.all_healthy().await {
                    tracing::warn!(status = ?health.status, "not all processes healthy");
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("health loop shutting down");
                break;
            }
        }
    }
}

async fn graceful_shutdown(
    supervisor: &KernelSupervisor,
) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown_order = [
        ProcessId::EmbeddingAdapter,
        ProcessId::ModelAdapter,
        ProcessId::Loop2Harness,
        ProcessId::Loop1Agentic,
        ProcessId::Loop0Meta,
        ProcessId::Kernel,
        ProcessId::Init,
    ];

    for pid in shutdown_order {
        tracing::info!(?pid, "stopping");
        supervisor
            .update_state(pid, ProcessState::Stopping)
            .await?;
        // TODO: send cancellation signal to actual loop task and await drain
        supervisor
            .update_state(pid, ProcessState::Stopped)
            .await?;
    }

    tracing::info!("all processes stopped");
    Ok(())
}

async fn signal_handler(shutdown_tx: watch::Sender<bool>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl_c");
        tracing::info!("received ctrl-c");
    }

    let _ = shutdown_tx.send(true);
}
