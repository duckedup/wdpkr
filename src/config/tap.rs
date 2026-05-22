use std::collections::HashMap;

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
