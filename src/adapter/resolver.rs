//! Provider resolution algorithm — deterministic selection based on
//! capabilities ∩ zone_allowed ∩ healthy → best_available.

use crate::envelope::task_envelope::SecurityZone;

use super::config::ProviderConfig;
use super::error::AdapterError;
use super::request::{AdapterRequest, CapabilityReq};
use super::DegradationLevel;

/// Result of provider resolution.
#[derive(Debug)]
pub struct ResolvedProvider {
    pub config: ProviderConfig,
    pub degradation_level: DegradationLevel,
}

/// Resolve the best available provider for a request.
///
/// Algorithm:
/// 1. Filter by security zone
/// 2. Filter by capabilities
/// 3. Filter by circuit breaker health (caller provides available set)
/// 4. Filter by adversarial constraint
/// 5. Sort by fallback rank
/// 6. Return the best match with degradation level
pub fn resolve(
    req: &AdapterRequest,
    providers: &[ProviderConfig],
    available_ids: &[String],
    last_provider_id: Option<&str>,
) -> Result<ResolvedProvider, AdapterError> {
    let mut candidates: Vec<(&ProviderConfig, DegradationLevel)> = providers
        .iter()
        // Step 1: Filter by security zone
        .filter(|p| zone_allows(req.security_zone, &p.zone_allowlist))
        // Step 2: Filter by capabilities
        .filter(|p| meets_capabilities(&req.capabilities, p))
        // Step 3: Filter by availability (circuit breaker)
        .filter(|p| available_ids.contains(&p.id))
        // Step 4: Filter by adversarial constraint
        .filter(|p| {
            if req.require_different_provider {
                last_provider_id.is_none_or(|last| last != p.id)
            } else {
                true
            }
        })
        .map(|p| {
            // Determine degradation level based on fallback rank
            let degradation = match p.fallback_rank {
                0..=1 => DegradationLevel::None,
                2 => DegradationLevel::Mild,
                _ => DegradationLevel::Significant,
            };
            (p, degradation)
        })
        .collect();

    // Step 5: Sort by fallback rank (ascending)
    candidates.sort_by_key(|(p, _)| p.fallback_rank);

    candidates
        .first()
        .map(|(p, d)| ResolvedProvider {
            config: (*p).clone(),
            degradation_level: *d,
        })
        .ok_or(AdapterError::NoProviderAvailable {
            zone: req.security_zone,
            reason: format!(
                "No provider meets requirements: zone={:?}, min_quality={:?}, tool_use={}, vision={}",
                req.security_zone,
                req.capabilities.min_quality_tier,
                req.capabilities.tool_use,
                req.capabilities.vision,
            ),
        })
}

/// Check if a security zone is in the provider's allowlist.
fn zone_allows(zone: SecurityZone, allowlist: &[SecurityZone]) -> bool {
    allowlist.contains(&zone)
}

/// Check if a provider meets the capability requirements.
fn meets_capabilities(req: &CapabilityReq, provider: &ProviderConfig) -> bool {
    let caps = &provider.capabilities;

    if caps.max_context < req.min_context {
        return false;
    }
    if req.supports_streaming && !caps.streaming {
        return false;
    }
    if req.tool_use && !caps.tool_use {
        return false;
    }
    if req.vision && !caps.vision {
        return false;
    }
    if caps.max_quality_tier < req.min_quality_tier {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::config::{ProviderCapabilities, ProviderConfig};
    use crate::adapter::request::{Message, QualityTier};
    use crate::envelope::task_envelope::Priority;

    fn anthropic_provider() -> ProviderConfig {
        ProviderConfig {
            id: "anthropic".to_string(),
            endpoint: "https://api.anthropic.com".to_string(),
            models: vec!["claude-opus-4-6".to_string()],
            fallback_rank: 1,
            zone_allowlist: vec![SecurityZone::Green, SecurityZone::Yellow],
            capabilities: ProviderCapabilities {
                max_context: 200_000,
                tool_use: true,
                vision: true,
                streaming: true,
                max_quality_tier: QualityTier::Opus,
            },
        }
    }

    fn ollama_provider() -> ProviderConfig {
        ProviderConfig {
            id: "ollama-local".to_string(),
            endpoint: "http://localhost:11434".to_string(),
            models: vec!["llama3.3-70b".to_string()],
            fallback_rank: 2,
            zone_allowlist: vec![SecurityZone::Green, SecurityZone::Yellow, SecurityZone::Red],
            capabilities: ProviderCapabilities {
                max_context: 128_000,
                tool_use: true,
                vision: false,
                streaming: true,
                max_quality_tier: QualityTier::Sonnet,
            },
        }
    }

    fn simple_request(zone: SecurityZone) -> AdapterRequest {
        AdapterRequest::simple(
            vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            Priority::P1Normal,
            zone,
        )
    }

    #[test]
    fn test_resolves_primary_provider() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["anthropic".to_string(), "ollama-local".to_string()];
        let req = simple_request(SecurityZone::Green);

        let result = resolve(&req, &providers, &available, None).unwrap();
        assert_eq!(result.config.id, "anthropic");
        assert_eq!(result.degradation_level, DegradationLevel::None);
    }

    #[test]
    fn test_falls_back_when_primary_unavailable() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["ollama-local".to_string()]; // anthropic is down

        let req = simple_request(SecurityZone::Green);
        let result = resolve(&req, &providers, &available, None).unwrap();
        assert_eq!(result.config.id, "ollama-local");
        assert_eq!(result.degradation_level, DegradationLevel::Mild);
    }

    #[test]
    fn test_red_zone_only_local() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["anthropic".to_string(), "ollama-local".to_string()];

        let req = simple_request(SecurityZone::Red);
        let result = resolve(&req, &providers, &available, None).unwrap();
        assert_eq!(result.config.id, "ollama-local");
    }

    #[test]
    fn test_no_provider_available() {
        let providers = vec![anthropic_provider()];
        let available = vec!["anthropic".to_string()];

        let req = simple_request(SecurityZone::Red); // Anthropic not in Red zone
        let result = resolve(&req, &providers, &available, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_adversarial_different_provider() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["anthropic".to_string(), "ollama-local".to_string()];

        let mut req = simple_request(SecurityZone::Green);
        req.require_different_provider = true;

        let result = resolve(&req, &providers, &available, Some("anthropic")).unwrap();
        assert_eq!(result.config.id, "ollama-local");
    }

    #[test]
    fn test_vision_requirement_filters() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["anthropic".to_string(), "ollama-local".to_string()];

        let mut req = simple_request(SecurityZone::Green);
        req.capabilities.vision = true;

        let result = resolve(&req, &providers, &available, None).unwrap();
        assert_eq!(result.config.id, "anthropic"); // Only anthropic has vision
    }

    #[test]
    fn test_quality_tier_filters() {
        let providers = vec![anthropic_provider(), ollama_provider()];
        let available = vec!["anthropic".to_string(), "ollama-local".to_string()];

        let mut req = simple_request(SecurityZone::Green);
        req.capabilities.min_quality_tier = QualityTier::Opus;

        let result = resolve(&req, &providers, &available, None).unwrap();
        assert_eq!(result.config.id, "anthropic"); // Only anthropic has Opus
    }
}
