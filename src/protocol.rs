//! Versioned client-facing protocol types shared by native and browser hosts.

use serde::{Deserialize, Serialize};

use crate::runtime::ResourceLimits;

pub const API_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureStatus {
    pub available: bool,
    pub durable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemFeatures {
    pub workspaces: FeatureStatus,
    pub capability_gatekeepers: FeatureStatus,
    pub bytecode_executor: FeatureStatus,
    pub wasm_executor: FeatureStatus,
    pub native_sandbox_executor: FeatureStatus,
    pub sessions: FeatureStatus,
    pub workflows: FeatureStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub product: String,
    pub version: String,
    pub api_version: String,
    pub runtime_limits: ResourceLimits,
    pub features: SystemFeatures,
}

impl SystemInfo {
    pub fn current() -> Self {
        Self {
            product: "A/RVM".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: API_VERSION.to_owned(),
            runtime_limits: ResourceLimits::default(),
            features: SystemFeatures {
                workspaces: FeatureStatus {
                    available: true,
                    durable: false,
                },
                capability_gatekeepers: FeatureStatus {
                    available: true,
                    durable: false,
                },
                bytecode_executor: FeatureStatus {
                    available: true,
                    durable: false,
                },
                wasm_executor: FeatureStatus {
                    available: false,
                    durable: false,
                },
                native_sandbox_executor: FeatureStatus {
                    available: false,
                    durable: false,
                },
                sessions: FeatureStatus {
                    available: true,
                    durable: true,
                },
                workflows: FeatureStatus {
                    available: false,
                    durable: false,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{API_VERSION, SystemInfo};

    #[test]
    fn system_info_round_trips_as_versioned_json() {
        let info = SystemInfo::current();
        let json = serde_json::to_string(&info).unwrap();
        let decoded: SystemInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, info);
        assert_eq!(decoded.api_version, API_VERSION);
        assert!(!decoded.features.wasm_executor.available);
    }
}
