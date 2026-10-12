use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Marker label put on every machine democtl creates.
pub const LABEL_MANAGED: &str = "democtl";
/// Label carrying the demo name a machine belongs to.
pub const LABEL_DEMO: &str = "demo";
/// Label carrying the per-demo creation index.
pub const LABEL_IDX: &str = "idx";
/// Label marking the golden-image template machine.
pub const LABEL_ROLE: &str = "role";
pub const ROLE_TEMPLATE: &str = "template";

#[derive(Debug, Clone)]
pub struct TemplateCfg {
    pub image: String,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
    pub packages: Vec<String>,
    /// Extra shell script run inside the template after the default setup.
    pub setup: Option<String>,
    pub shell: String,
    pub workspace: String,
}

#[derive(Debug, Clone)]
pub struct DefaultsCfg {
    pub net: bool,
    pub ssh_agent: bool,
    pub secret_env: Vec<String>,
    pub guest_port: u16,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DemoCfg {
    pub port: Option<u16>,
    pub guest_port: Option<u16>,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
    pub secret_env: Option<Vec<String>>,
}

/// A demo with every default from `[defaults]` resolved.
#[derive(Debug, Clone)]
pub struct EffectiveDemo {
    pub name: String,
    pub host_port: u16,
    pub guest_port: u16,
    pub net: bool,
    pub ssh_agent: bool,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
    pub secret_env: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub template: TemplateCfg,
    pub defaults: DefaultsCfg,
    pub demos: BTreeMap<String, DemoCfg>,
}

impl Config {
    pub fn demo(&self, name: &str) -> Result<&DemoCfg> {
        self.demos.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.demos.keys().map(String::as_str).collect();
            anyhow::anyhow!(
                "unknown demo `{name}` — configured demos: {}. Add it to [demos.{name}] in demo.toml",
                if names.is_empty() { "(none)".to_string() } else { names.join(", ") }
            )
        })
    }

    pub fn effective(&self, name: &str) -> Result<EffectiveDemo> {
        let d = self.demo(name)?;
        let port = d.port.ok_or_else(|| {
            anyhow::anyhow!(
                "demo `{name}` has no fixed host port — set `port = 8410` (or similar) in [demos.{name}]"
            )
        })?;
        Ok(EffectiveDemo {
            name: name.to_string(),
            host_port: port,
            guest_port: d.guest_port.unwrap_or(self.defaults.guest_port),
            net: self.defaults.net,
            ssh_agent: self.defaults.ssh_agent,
            cpus: d.cpus.or(self.defaults.cpus),
            mem_mib: d.mem_mib.or(self.defaults.mem_mib),
            secret_env: d.secret_env.clone().unwrap_or_else(|| self.defaults.secret_env.clone()),
        })
    }
}

/// Everything a command needs: parsed config plus where it lives.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// Per-project scratch/state directory (`<dir>/.democtl`).
    pub dot_dir: PathBuf,
    /// Name of the golden-image template machine.
    pub template_name: String,
    /// Path of the packed golden image.
    pub golden_path: PathBuf,
}

#[derive(Deserialize, Debug, Default)]
struct RawConfig {
    template: Option<RawTemplate>,
    defaults: Option<RawDefaults>,
    #[serde(default)]
    demos: BTreeMap<String, RawDemo>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawTemplate {
    image: Option<String>,
    cpus: Option<u32>,
    mem: Option<u32>,
    packages: Option<Vec<String>>,
    setup: Option<String>,
    shell: Option<String>,
    workspace: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawDefaults {
    net: Option<bool>,
    ssh_agent: Option<bool>,
    secret_env: Option<Vec<String>>,
    guest_port: Option<u16>,
    cpus: Option<u32>,
    mem: Option<u32>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawDemo {
    port: Option<u16>,
    guest_port: Option<u16>,
    cpus: Option<u32>,
    mem: Option<u32>,
    secret_env: Option<Vec<String>>,
}

pub fn default_packages() -> Vec<String> {
    ["ca-certificates", "curl", "git", "jq", "less", "ripgrep"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        let t = raw.template.unwrap_or_default();
        let d = raw.defaults.unwrap_or_default();
        Config {
            template: TemplateCfg {
                image: t.image.unwrap_or_else(|| "debian:bookworm-slim".into()),
                cpus: t.cpus,
                mem_mib: t.mem,
                packages: t.packages.unwrap_or_else(default_packages),
                setup: t.setup,
                shell: t.shell.unwrap_or_else(|| "/bin/bash".into()),
                workspace: t.workspace.unwrap_or_else(|| "/workspace".into()),
            },
            defaults: DefaultsCfg {
                net: d.net.unwrap_or(true),
                ssh_agent: d.ssh_agent.unwrap_or(true),
                secret_env: d.secret_env.unwrap_or_default(),
                guest_port: d.guest_port.unwrap_or(8080),
                cpus: d.cpus,
                mem_mib: d.mem,
            },
            demos: raw
                .demos
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        DemoCfg {
                            port: v.port,
                            guest_port: v.guest_port,
                            cpus: v.cpus,
                            mem_mib: v.mem,
                            secret_env: v.secret_env,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Find demo.toml: the explicit path, the current directory, or the nearest
/// ancestor directory that contains one.
pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let path = match explicit {
        Some(p) => {
            if !p.is_file() {
                bail!("config not found: {}", p.display());
            }
            p.to_path_buf()
        }
        None => {
            let cwd = std::env::current_dir().context("resolving current directory")?;
            find_up(&cwd).ok_or_else(|| {
                anyhow::anyhow!(
                    "no demo.toml found in {} or any parent — run `democtl init` or pass --config",
                    cwd.display()
                )
            })?
        }
    };
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let raw: RawConfig = toml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    let config: Config = raw.into();

    let dot_dir = dir.join(".democtl");
    let golden_path = dot_dir.join("golden.smolmachine");
    let template_name = format!("democtl-template-{}", slug(&dir));
    Ok(Loaded {
        config,
        dot_dir,
        template_name,
        golden_path,
    })
}

fn find_up(from: &Path) -> Option<PathBuf> {
    let mut cur = Some(from.to_path_buf());
    while let Some(dir) = cur {
        let candidate = dir.join("demo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// Filesystem-safe slug for machine names: lowercase, `[^a-z0-9-]` → `-`.
pub fn slug(s: &Path) -> String {
    let name = s.file_name().and_then(|n| n.to_str()).unwrap_or("default");
    crate::util::ident(name)
}

pub const STARTER: &str = r#"# democtl configuration — one section per conference demo.
# Full reference in the project README.

[defaults]
guest_port = 8080                 # port your webapp listens on inside the VM
secret_env = ["ANTHROPIC_API_KEY", "OPENROUTER_API_KEY"]  # forwarded into each VM as secrets
# net = true                      # outbound networking (default true)
# ssh_agent = true                # forward your SSH agent for git pushes (default true)

[demos.checkout]
port = 8410                       # the demo is ALWAYS at http://localhost:8410

[demos.search]
port = 8420
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_clean() {
        assert_eq!(slug(Path::new("/a/b/My Project")), "my-project");
        assert_eq!(slug(Path::new("/atomic")), "atomic");
        assert_eq!(slug(Path::new("/")), "default");
    }

    #[test]
    fn parses_starter() {
        let raw: RawConfig = toml::from_str(STARTER).unwrap();
        let cfg: Config = raw.into();
        assert_eq!(cfg.demos.len(), 2);
        let eff = cfg.effective("checkout").unwrap();
        assert_eq!(eff.host_port, 8410);
        assert_eq!(eff.guest_port, 8080);
        assert!(eff.net);
    }

    #[test]
    fn missing_port_is_an_error() {
        let raw: RawConfig = toml::from_str("[demos.x]\n").unwrap();
        let cfg: Config = raw.into();
        assert!(cfg.effective("x").is_err());
    }
}
