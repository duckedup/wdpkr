use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{FileConfig, Source};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePluginConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Clone)]
pub struct PluginConfig {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub settings: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone)]
pub struct PluginsSources {
    pub source: Source,
}

pub fn resolve(file: &Option<FileConfig>) -> (Vec<PluginConfig>, PluginsSources) {
    match file {
        Some(fc) if fc.plugins.as_ref().is_some_and(|p| !p.is_empty()) => {
            let plugins = fc
                .plugins
                .as_ref()
                .unwrap()
                .iter()
                .map(|fp| PluginConfig {
                    name: fp.name.clone(),
                    command: fp.command.clone(),
                    args: fp.args.clone().unwrap_or_default(),
                    settings: fp.settings.clone().unwrap_or_default(),
                })
                .collect();
            (
                plugins,
                PluginsSources {
                    source: Source::File,
                },
            )
        }
        _ => (
            vec![PluginConfig {
                name: "files".into(),
                command: None,
                args: vec![],
                settings: HashMap::new(),
            }],
            PluginsSources {
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
    fn absent_plugins_defaults_to_files() {
        let (plugins, sources) = resolve(&None);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "files");
        assert_eq!(sources.source, Source::Default);
    }

    #[test]
    fn empty_plugins_defaults_to_files() {
        let file = FileConfig {
            plugins: Some(vec![]),
            ..Default::default()
        };
        let (plugins, sources) = resolve(&Some(file));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "files");
        assert_eq!(sources.source, Source::Default);
    }

    #[test]
    fn explicit_files_plugin() {
        let file = FileConfig {
            plugins: Some(vec![FilePluginConfig {
                name: "files".into(),
                command: None,
                args: None,
                settings: None,
            }]),
            ..Default::default()
        };
        let (plugins, sources) = resolve(&Some(file));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "files");
        assert_eq!(sources.source, Source::File);
    }

    #[test]
    fn multiple_plugins_parse() {
        let file = FileConfig {
            plugins: Some(vec![
                FilePluginConfig {
                    name: "files".into(),
                    command: None,
                    args: None,
                    settings: None,
                },
                FilePluginConfig {
                    name: "linear".into(),
                    command: Some("/usr/bin/linear-plugin".into()),
                    args: Some(vec!["--team".into(), "ENG".into()]),
                    settings: Some(HashMap::from([(
                        "api_key_env".into(),
                        serde_yaml::Value::String("LINEAR_API_KEY".into()),
                    )])),
                },
            ]),
            ..Default::default()
        };
        let (plugins, sources) = resolve(&Some(file));
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "files");
        assert_eq!(plugins[1].name, "linear");
        assert_eq!(
            plugins[1].command.as_deref(),
            Some("/usr/bin/linear-plugin")
        );
        assert_eq!(plugins[1].args, vec!["--team", "ENG"]);
        assert!(plugins[1].settings.contains_key("api_key_env"));
        assert_eq!(sources.source, Source::File);
    }

    #[test]
    fn plugin_with_command_and_settings_yaml_round_trip() {
        let yaml = r#"
plugins:
  - name: custom-tool
    command: /path/to/plugin
    args: ["--flag"]
    settings:
      key: value
      count: 42
"#;
        let parsed: FileConfig = serde_yaml::from_str(yaml).expect("yaml parses");
        let plugins = parsed.plugins.as_ref().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "custom-tool");
        assert_eq!(plugins[0].command.as_deref(), Some("/path/to/plugin"));
        assert_eq!(
            plugins[0].args.as_ref().unwrap(),
            &vec!["--flag".to_string()]
        );
        let settings = plugins[0].settings.as_ref().unwrap();
        assert_eq!(
            settings.get("key"),
            Some(&serde_yaml::Value::String("value".into()))
        );
    }

    #[test]
    fn default_config_yaml_still_parses() {
        let parsed: FileConfig =
            serde_yaml::from_str(crate::config::DEFAULT_CONFIG_YAML).expect("default yaml parses");
        assert!(
            parsed.plugins.is_none(),
            "default config should not have plugins"
        );
    }
}
