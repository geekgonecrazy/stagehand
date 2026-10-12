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
    pub credentials: BTreeMap<String, CredentialCfg>,
    pub guest_port: u16,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
}

/// A credential bound host-side: the VM sees a `SMOL_PLACEHOLDER_*` value in
/// the env var, and smolvm substitutes the real host-env value only on HTTPS
/// requests to `hosts`. The real value never enters the VM.
#[derive(Debug, Clone)]
pub struct CredentialCfg {
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DemoCfg {
    pub port: Option<u16>,
    pub guest_port: Option<u16>,
    pub cpus: Option<u32>,
    pub mem_mib: Option<u32>,
    pub secret_env: Option<Vec<String>>,
    pub credentials: Option<BTreeMap<String, CredentialCfg>>,
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
    /// credential env var name → hosts it may be used with
    pub credentials: BTreeMap<String, Vec<String>>,
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
            credentials: d
                .credentials
                .clone()
                .unwrap_or_else(|| self.defaults.credentials.clone())
                .into_iter()
                .map(|(k, v)| (k, v.hosts))
                .collect(),
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
    /// Path of the packed golden image (build intermediate).
    pub golden_path: PathBuf,
    /// Unpacked golden rootfs — what demo machines are created from
    /// (`machine create --image`, which honors credential bindings).
    pub rootfs_path: PathBuf,
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
    credentials: Option<BTreeMap<String, RawCredential>>,
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
    credentials: Option<BTreeMap<String, RawCredential>>,
}

#[derive(Deserialize, Debug, Default)]
struct RawCredential {
    hosts: Option<Vec<String>>,
}

impl RawCredential {
    fn into_cfg(self, name: &str) -> Result<CredentialCfg> {
        let hosts = self.hosts.unwrap_or_default();
        if hosts.is_empty() {
            bail!(
                "credential `{name}` needs at least one host, e.g. `hosts = [\"api.anthropic.com\"]`"
            );
        }
        Ok(CredentialCfg { hosts })
    }
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
        let parse_creds = |m: Option<BTreeMap<String, RawCredential>>| -> Vec<(String, Result<CredentialCfg>)> {
            m.unwrap_or_default()
                .into_iter()
                .map(|(k, v)| {
                    let r = v.into_cfg(&k);
                    (k, r)
                })
                .collect()
        };
        let mut defaults_credentials = BTreeMap::new();
        for (k, r) in parse_creds(d.credentials) {
            match r {
                Ok(c) => {
                    defaults_credentials.insert(k, c);
                }
                Err(e) => eprintln!("warning: [defaults.credentials.{k}]: {e}"),
            }
        }
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
                credentials: defaults_credentials,
                guest_port: d.guest_port.unwrap_or(8080),
                cpus: d.cpus,
                mem_mib: d.mem,
            },
            demos: raw
                .demos
                .into_iter()
                .map(|(k, v)| {
                    let credentials = match v.credentials {
                        // per-demo credentials REPLACE the defaults when present
                        Some(m) => {
                            let mut out = BTreeMap::new();
                            for (ck, r) in parse_creds(Some(m)) {
                                if let Ok(c) = r {
                                    out.insert(ck, c);
                                }
                            }
                            Some(out)
                        }
                        None => None,
                    };
                    (
                        k,
                        DemoCfg {
                            port: v.port,
                            guest_port: v.guest_port,
                            cpus: v.cpus,
                            mem_mib: v.mem,
                            secret_env: v.secret_env,
                            credentials,
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
    let rootfs_path = dot_dir.join("rootfs");
    let template_name = format!("democtl-template-{}", slug(&dir));
    Ok(Loaded {
        config,
        dot_dir,
        template_name,
        golden_path,
        rootfs_path,
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

# Credentials bound HOST-SIDE: the VM only ever sees a SMOL_PLACEHOLDER_*
# value; smolvm substitutes the real value from your shell env on HTTPS
# requests to the hosts you list. The secret itself never enters the VM.
# List EVERY host the credential is sent to, matching what the client
# really connects to (e.g. OpenRouter's API host is "openrouter.ai").
# The var must be set in your shell when a machine is STARTED.
#
# [defaults.credentials.OPENROUTER_API_KEY]
# hosts = ["openrouter.ai"]

# For secrets that must be plainly visible inside the VM (non-HTTPS use):
# secret_env = ["SOME_PLAIN_TOKEN"]
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

    #[test]
    fn parses_credentials() {
        let raw: RawConfig = toml::from_str(
            "[defaults.credentials.ANTHROPIC_API_KEY]\nhosts = [\"api.anthropic.com\"]\n\n[demos.y]\nport = 9000\n",
        )
        .unwrap();
        let cfg: Config = raw.into();
        let eff = cfg.effective("y").unwrap();
        assert_eq!(
            eff.credentials.get("ANTHROPIC_API_KEY").map(Vec::as_slice),
            Some(["api.anthropic.com".to_string()].as_slice())
        );
        // per-demo credentials replace defaults
        let raw: RawConfig = toml::from_str(
            "[defaults.credentials.A]\nhosts = [\"a.example\"]\n\n[demos.y.credentials.B]\nhosts = [\"b.example\"]\n\n[demos.y]\nport = 9000\n",
        )
        .unwrap();
        let cfg: Config = raw.into();
        let eff = cfg.effective("y").unwrap();
        assert!(eff.credentials.contains_key("B"));
        assert!(!eff.credentials.contains_key("A"));
    }

    #[test]
    fn credential_without_hosts_is_rejected() {
        let raw: RawConfig = toml::from_str("[defaults.credentials.X]\n").unwrap();
        let cfg: Config = raw.into();
        assert!(cfg.defaults.credentials.is_empty());
    }
}
