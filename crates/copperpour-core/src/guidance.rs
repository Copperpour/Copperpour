//! Read-only drift report for the AI guidance installed beside this server (#728).
//!
//! The standalone binary embeds the bundle that `konnect init` writes, so it
//! supplies the probe; core only carries it to `get_installation_info` and the
//! `initialize` result. Nothing here installs, restores, or rewrites guidance
//! (#242).

use serde_json::Value;

pub trait GuidanceProbe: Send + Sync {
    /// The `guidance` block of `get_installation_info`.
    fn detail(&self) -> Value;
    /// A notice for the model when installed guidance differs from the bundle
    /// this server carries. The probe hands it out at most once for each
    /// installed version, so a later call or a later start gets `None`.
    fn take_notice(&self) -> Option<String>;
}
