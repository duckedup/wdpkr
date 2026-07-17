use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::{FileConfig, Source};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTapConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Clone)]
pub struct TapConfig {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub settings: HashMap<String, serde_yaml::Value>,
}

impl TapConfig {
    /// Parse this tap's `settings.decay` block into a [`DecayConfig`]. Absent →
    /// decay disabled. Used by the search layer to re-rank results by age.
    pub fn decay(&self) -> Result<DecayConfig> {
        DecayConfig::from_settings(&self.settings)
    }
}

/// Per-tap search-time decay parameters. Opt-in: absent `decay` block → disabled
/// (score unchanged). When enabled, a result's cosine score is multiplied by
/// `max(floor, 0.5 ^ (age_days / half_life_days))`, so stale, un-reinforced
/// documents sink in ranking but never below `floor` (never vanish).
#[derive(Debug, Clone, PartialEq)]
pub struct DecayConfig {
    pub enabled: bool,
    pub half_life_days: f64,
    pub floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            half_life_days: 90.0,
            floor: 0.4,
        }
    }
}

impl DecayConfig {
    /// Parse from a tap's untyped `settings` map. A missing `decay` key yields
    /// the disabled default. When the block is present, decay is enabled unless
    /// `enabled: false` is set explicitly. Validates `half_life_days > 0` and
    /// `0.0 <= floor <= 1.0`.
    pub fn from_settings(settings: &HashMap<String, serde_yaml::Value>) -> Result<Self> {
        let Some(block) = settings.get("decay") else {
            return Ok(Self::default());
        };
        let map = block
            .as_mapping()
            .ok_or_else(|| anyhow!("tap 'decay' must be a mapping"))?;

        let mut out = Self {
            enabled: true, // presence of the block opts in
            ..Self::default()
        };
        if let Some(v) = map.get(serde_yaml::Value::from("enabled")) {
            out.enabled = v
                .as_bool()
                .ok_or_else(|| anyhow!("tap decay 'enabled' must be a boolean"))?;
        }
        if let Some(v) = map.get(serde_yaml::Value::from("half_life_days")) {
            out.half_life_days = v
                .as_f64()
                .ok_or_else(|| anyhow!("tap decay 'half_life_days' must be a number"))?;
            if out.half_life_days <= 0.0 {
                return Err(anyhow!("tap decay 'half_life_days' must be > 0"));
            }
        }
        if let Some(v) = map.get(serde_yaml::Value::from("floor")) {
            out.floor = v
                .as_f64()
                .ok_or_else(|| anyhow!("tap decay 'floor' must be a number"))?;
            if !(0.0..=1.0).contains(&out.floor) {
                return Err(anyhow!("tap decay 'floor' must be between 0.0 and 1.0"));
            }
        }
        Ok(out)
    }

    /// Multiplier to apply to a result's score given the document's age in
    /// seconds. `1.0` when disabled or age unknown.
    pub fn multiplier(&self, age_secs: i64) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let age_days = (age_secs.max(0) as f64) / 86_400.0;
        let m = 0.5_f64.powf(age_days / self.half_life_days);
        m.max(self.floor) as f32
    }
}

#[derive(Debug, Clone)]
pub struct TapsSources {
    pub source: Source,
}

pub fn resolve(file: &Option<FileConfig>) -> (Vec<TapConfig>, TapsSources) {
    match file {
        Some(fc) if fc.taps.as_ref().is_some_and(|p| !p.is_empty()) => {
            let taps = fc
                .taps
                .as_ref()
                .unwrap()
                .iter()
                .map(|fp| TapConfig {
                    name: fp.name.clone(),
                    command: fp.command.clone(),
                    args: fp.args.clone().unwrap_or_default(),
                    settings: fp.settings.clone().unwrap_or_default(),
                })
                .collect();
            (
                taps,
                TapsSources {
                    source: Source::File,
                },
            )
        }
        _ => (
            vec![TapConfig {
                name: "files".into(),
                command: None,
                args: vec![],
                settings: HashMap::new(),
            }],
            TapsSources {
                source: Source::Default,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileConfig;

    // ── DecayConfig ───────────────────────────────────────────────────

    fn decay_settings(yaml: &str) -> HashMap<String, serde_yaml::Value> {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn decay_absent_is_disabled() {
        let d = DecayConfig::from_settings(&HashMap::new()).unwrap();
        assert!(!d.enabled);
        assert_eq!(d.multiplier(10_000_000), 1.0);
    }

    #[test]
    fn decay_block_present_enables_with_defaults() {
        let d = DecayConfig::from_settings(&decay_settings("decay: {}")).unwrap();
        assert!(d.enabled);
        assert_eq!(d.half_life_days, 90.0);
        assert_eq!(d.floor, 0.4);
    }

    #[test]
    fn decay_overrides_parse() {
        let d = DecayConfig::from_settings(&decay_settings(
            "decay:\n  half_life_days: 30\n  floor: 0.5",
        ))
        .unwrap();
        assert!(d.enabled);
        assert_eq!(d.half_life_days, 30.0);
        assert_eq!(d.floor, 0.5);
    }

    #[test]
    fn decay_explicit_disable() {
        let d = DecayConfig::from_settings(&decay_settings("decay:\n  enabled: false")).unwrap();
        assert!(!d.enabled);
    }

    #[test]
    fn decay_rejects_bad_values() {
        assert!(
            DecayConfig::from_settings(&decay_settings("decay:\n  half_life_days: 0")).is_err()
        );
        assert!(DecayConfig::from_settings(&decay_settings("decay:\n  floor: 1.5")).is_err());
        assert!(DecayConfig::from_settings(&decay_settings("decay: 7")).is_err());
    }

    #[test]
    fn decay_multiplier_curve() {
        let d = DecayConfig {
            enabled: true,
            half_life_days: 90.0,
            floor: 0.4,
        };
        // Fresh → ~1.0
        assert!((d.multiplier(0) - 1.0).abs() < 1e-6);
        // One half-life (90d) → ~0.5
        assert!((d.multiplier(90 * 86_400) - 0.5).abs() < 1e-3);
        // Very old → floored at 0.4
        assert_eq!(d.multiplier(10_000 * 86_400), 0.4);
    }

    #[test]
    fn tap_config_decay_helper() {
        let tap = TapConfig {
            name: "notion".into(),
            command: None,
            args: vec![],
            settings: decay_settings("decay:\n  half_life_days: 45"),
        };
        assert_eq!(tap.decay().unwrap().half_life_days, 45.0);
    }

    #[test]
    fn absent_taps_defaults_to_files() {
        let (taps, sources) = resolve(&None);
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name, "files");
        assert_eq!(sources.source, Source::Default);
    }

    #[test]
    fn empty_taps_defaults_to_files() {
        let file = FileConfig {
            taps: Some(vec![]),
            ..Default::default()
        };
        let (taps, sources) = resolve(&Some(file));
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name, "files");
        assert_eq!(sources.source, Source::Default);
    }

    #[test]
    fn explicit_files_tap() {
        let file = FileConfig {
            taps: Some(vec![FileTapConfig {
                name: "files".into(),
                command: None,
                args: None,
                settings: None,
            }]),
            ..Default::default()
        };
        let (taps, sources) = resolve(&Some(file));
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name, "files");
        assert_eq!(sources.source, Source::File);
    }

    #[test]
    fn multiple_taps_parse() {
        let file = FileConfig {
            taps: Some(vec![
                FileTapConfig {
                    name: "files".into(),
                    command: None,
                    args: None,
                    settings: None,
                },
                FileTapConfig {
                    name: "linear".into(),
                    command: Some("/usr/bin/linear-tap".into()),
                    args: Some(vec!["--team".into(), "ENG".into()]),
                    settings: Some(HashMap::from([(
                        "api_key_env".into(),
                        serde_yaml::Value::String("LINEAR_API_KEY".into()),
                    )])),
                },
            ]),
            ..Default::default()
        };
        let (taps, sources) = resolve(&Some(file));
        assert_eq!(taps.len(), 2);
        assert_eq!(taps[0].name, "files");
        assert_eq!(taps[1].name, "linear");
        assert_eq!(taps[1].command.as_deref(), Some("/usr/bin/linear-tap"));
        assert_eq!(taps[1].args, vec!["--team", "ENG"]);
        assert!(taps[1].settings.contains_key("api_key_env"));
        assert_eq!(sources.source, Source::File);
    }

    #[test]
    fn tap_with_command_and_settings_yaml_round_trip() {
        let yaml = r#"
taps:
  - name: custom-tool
    command: /path/to/tap
    args: ["--flag"]
    settings:
      key: value
      count: 42
"#;
        let parsed: FileConfig = serde_yaml::from_str(yaml).expect("yaml parses");
        let taps = parsed.taps.as_ref().unwrap();
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name, "custom-tool");
        assert_eq!(taps[0].command.as_deref(), Some("/path/to/tap"));
        assert_eq!(taps[0].args.as_ref().unwrap(), &vec!["--flag".to_string()]);
        let settings = taps[0].settings.as_ref().unwrap();
        assert_eq!(
            settings.get("key"),
            Some(&serde_yaml::Value::String("value".into()))
        );
    }

    #[test]
    fn default_config_yaml_still_parses() {
        let parsed: FileConfig =
            serde_yaml::from_str(crate::config::DEFAULT_CONFIG_YAML).expect("default yaml parses");
        assert!(parsed.taps.is_none(), "default config should not have taps");
    }
}
