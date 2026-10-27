use anyhow::{Context, Result};
use ort::ep::CPU;
#[cfg(target_os = "macos")]
use ort::ep::{CoreML, ExecutionProvider};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrtProvider {
    Cpu,
    CoreMl,
}

impl fmt::Display for OrtProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
        })
    }
}

pub const DEFAULT_VAD_PROVIDER: OrtProvider = OrtProvider::Cpu;
pub const DEFAULT_ASR_PROVIDER: OrtProvider = OrtProvider::Cpu;

#[derive(Clone, Copy)]
pub struct SessionConfig<'a> {
    pub label: &'a str,
    pub provider: OrtProvider,
    pub intra_threads: Option<usize>,
    pub profiling_path: Option<&'a Path>,
}

pub struct InitializedSession {
    session: Session,
    provider: OrtProvider,
    init_duration: Duration,
}

impl InitializedSession {
    pub fn provider(&self) -> OrtProvider {
        self.provider
    }

    pub fn init_duration(&self) -> Duration {
        self.init_duration
    }
}

impl Deref for InitializedSession {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl DerefMut for InitializedSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

pub struct ProviderReport<'a> {
    label: &'a str,
    provider: OrtProvider,
}

impl<'a> ProviderReport<'a> {
    pub fn new(label: &'a str, provider: OrtProvider) -> Self {
        Self { label, provider }
    }
}

impl fmt::Display for ProviderReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ORT session '{}' initialized provider: {}",
            self.label, self.provider
        )
    }
}

pub fn coreml_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        return CoreML::default().is_available().unwrap_or(false);
    }

    #[cfg(not(target_os = "macos"))]
    false
}

pub fn capability_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "cpu, coreml"
    } else {
        "cpu"
    }
}

pub fn configure_transcribe_provider(requested: OrtProvider) -> OrtProvider {
    use transcribe_rs::accel::{self, OrtAccelerator};

    let provider = match requested {
        OrtProvider::CoreMl if coreml_available() => OrtProvider::CoreMl,
        _ => OrtProvider::Cpu,
    };
    let accelerator = match provider {
        OrtProvider::Cpu => OrtAccelerator::CpuOnly,
        OrtProvider::CoreMl => OrtAccelerator::CoreMl,
    };
    accel::set_ort_accelerator(accelerator);
    provider
}

pub fn report_transcribe_session(label: &str, provider: OrtProvider) {
    match provider {
        OrtProvider::Cpu => log::info!("{}", ProviderReport::new(label, provider)),
        OrtProvider::CoreMl => log::info!(
            "ORT ASR session group '{}' initialized with CoreML capability; transcribe-rs 0.3.8 does not expose post-partition provider state or CPU fallback nodes",
            label
        ),
    }
}

pub fn build_session(path: &Path, config: SessionConfig<'_>) -> Result<InitializedSession> {
    let started = Instant::now();
    let (session, provider) = match config.provider {
        OrtProvider::Cpu => (
            build_exact(path, config, OrtProvider::Cpu)?,
            OrtProvider::Cpu,
        ),
        OrtProvider::CoreMl => match build_exact(path, config, OrtProvider::CoreMl) {
            Ok(session) => (session, OrtProvider::CoreMl),
            Err(error) => {
                log::warn!(
                    "ORT session '{}' could not initialize CoreML ({error:#}); falling back to CPU",
                    config.label
                );
                (
                    build_exact(path, config, OrtProvider::Cpu).with_context(|| {
                        format!(
                            "ORT session '{}' also failed to initialize its CPU fallback",
                            config.label
                        )
                    })?,
                    OrtProvider::Cpu,
                )
            }
        },
    };
    let initialized = InitializedSession {
        session,
        provider,
        init_duration: started.elapsed(),
    };
    log::info!(
        "{}",
        ProviderReport::new(config.label, initialized.provider())
    );
    Ok(initialized)
}

fn build_exact(path: &Path, config: SessionConfig<'_>, provider: OrtProvider) -> Result<Session> {
    let mut builder = Session::builder()
        .context("create ORT session builder")?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|error| anyhow::anyhow!("set ORT graph optimization level: {error}"))?;

    if let Some(threads) = config.intra_threads.filter(|threads| *threads > 0) {
        builder = builder
            .with_intra_threads(threads)
            .map_err(|error| anyhow::anyhow!("set ORT intra-op thread count: {error}"))?;
    }
    if let Some(profiling_path) = config.profiling_path {
        builder = builder
            .with_profiling(profiling_path)
            .map_err(|error| anyhow::anyhow!("enable ORT profiling: {error}"))?;
    }

    match provider {
        OrtProvider::Cpu => {
            builder = builder
                .with_execution_providers([CPU::default().build().error_on_failure()])
                .map_err(|error| anyhow::anyhow!("register CPU execution provider: {error}"))?;
        }
        OrtProvider::CoreMl => {
            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .with_execution_providers([
                        CoreML::default().build().error_on_failure(),
                        CPU::default().build().error_on_failure(),
                    ])
                    .map_err(|error| {
                        anyhow::anyhow!("register CoreML execution provider: {error}")
                    })?;
            }
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!("CoreML is unavailable on this target");
        }
    }

    builder
        .commit_from_file(path)
        .with_context(|| format!("load ORT model from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialized_cpu_session_drives_the_provider_log() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/ten-vad.onnx");
        let session = build_session(
            &path,
            SessionConfig {
                label: "provider-log-test",
                provider: OrtProvider::Cpu,
                intra_threads: Some(1),
                profiling_path: None,
            },
        )
        .expect("initialize CPU session");

        assert_eq!(session.provider(), OrtProvider::Cpu);
    }
}
