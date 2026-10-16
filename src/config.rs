use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Marker label put on every machine stagehand creates.
pub const LABEL_MANAGED: &str = "stagehand";
/// Label carrying the demo name a machine belongs to.
pub const LABEL_DEMO: &str = "demo";
/// Label carrying the per-demo creation index.
pub const LABEL_IDX: &str = "idx";
/// Label marking the golden-image template machine.
pub const LABEL_ROLE: &str = "role";
pub const ROLE_TEMPLATE: &str = "template";
/// Label marking the Linux build machine.
pub const ROLE_BUILDER: &str = "builder";

/// The golden image recipe (`[build]`): assembled by `stagehand build`,
/// cloned into every demo VM.
#[derive(Debug, Clone)]
pub struct BuildCfg {
    /// Base image for the template machine.
    pub image: String,
    /// Distro packages installed into the golden image.
    pub packages: Vec<String>,
    /// mise tool specs installed into the golden image (e.g. `node@lts`).
    /// Every binary they provide is put on PATH via mirrored shims.
    pub tools: Vec<String>,
    /// What `stagehand new`/`open` land in when no command override is given
    /// (e.g. `["some-cli", "--flag"]`). Defaults to `shell`.
    pub entrypoint: Vec<String>,
    /// Extra shell script run inside the template after the default setup.
    pub setup: Option<String>,
    /// Login shell used when the entrypoint is unset.
    pub shell: String,
    /// Working directory for new/open/exec inside the VM.
    pub workspace: String,
}

impl BuildCfg {
    /// The programs `new`/`open` attach to when not overridden per-invocation.
    pub fn entrypoint_programs(&self) -> Vec<String> {
        if self.entrypoint.is_empty() {
            vec![self.shell.clone()]
        } else {
            self.entrypoint.clone()
        }
    }
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

/// The demo itself: everything top-level that isn't image recipe or defaults.
#[derive(Debug, Clone)]
pub struct DemoCfg {
    /// Machine-name stem and display name (default: demo.toml's directory name).
    pub name: Option<String>,
    /// Fixed host port — the demo's permanent URL.
    pub port: Option<u16>,
    /// Host folders copied into the VM at `new` time (`/workspace/<basename>`).
    /// A fresh copy every run; the host original is never touched.
    pub include: Option<Vec<String>>,
}

/// The demo with every default from `[defaults]` resolved.
#[derive(Debug, Clone)]
pub struct EffectiveDemo {
    /// Machine-name stem: the slug of the directory holding demo.toml.
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
    /// Host folders to copy into the VM at `new` time (unresolved paths).
    pub include: Vec<String>,
}

/// Optional Linux artifact stage (`[builder]`): a separate machine that
/// cross-compiles artifacts (e.g. from a host checkout mounted at `/src`)
/// which `stagehand build` installs into the golden image.
#[derive(Debug, Clone)]
pub struct BuilderCfg {
    pub image: String,
    /// Host directory mounted read-write at `/src` inside the builder.
    pub source: PathBuf,
    /// mise tool specs installed in the builder before `script` runs
    /// (e.g. `rust@stable`). The shims dir is prepended to PATH.
    pub tools: Vec<String>,
    /// Shell script run inside the builder (cwd `/build`). Must place
    /// exportable files under `/build/artifacts/`.
    pub script: String,
    /// File names under `/build/artifacts/` to copy back to the host and
    /// install into the golden image at `/usr/local/bin/`.
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The golden image recipe.
    pub build: BuildCfg,
    /// Optional Linux artifact stage.
    pub builder: Option<BuilderCfg>,
    pub defaults: DefaultsCfg,
    /// The demo (top-level keys).
    pub demo: DemoCfg,
}

/// Everything a command needs: parsed config plus where it lives.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// Directory containing demo.toml — relative `include` paths resolve here.
    pub dir: PathBuf,
    /// Filesystem-safe stem for machine names (demo.toml's directory name).
    pub slug: String,
    /// Per-project scratch/state directory (`<dir>/.stagehand`).
    pub dot_dir: PathBuf,
    /// Name of the golden-image template machine.
    pub template_name: String,
    /// Name of the optional Linux build machine.
    pub builder_name: String,
    /// Path of the packed golden image (build intermediate).
    pub golden_path: PathBuf,
    /// Unpacked golden rootfs — what demo machines are created from
    /// (`machine create --image`, which honors credential bindings).
    pub rootfs_path: PathBuf,
    /// Where exported build artifacts land on the host.
    pub artifacts_dir: PathBuf,
}

#[derive(Deserialize, Debug, Default)]
struct RawConfig {
    /// The demo, declared at the top level.
    name: Option<String>,
    port: Option<u16>,
    include: Option<Vec<String>>,
    build: Option<RawBuild>,
    builder: Option<RawBuilder>,
    defaults: Option<RawDefaults>,
}

/// The golden image recipe.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawBuild {
    image: Option<String>,
    packages: Option<Vec<String>>,
    tools: Option<Vec<String>>,
    entrypoint: Option<OneOrMany>,
    setup: Option<String>,
    shell: Option<String>,
    workspace: Option<String>,
}

/// The optional cross-compile stage.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawBuilder {
    image: Option<String>,
    source: Option<PathBuf>,
    tools: Option<Vec<String>>,
    script: Option<String>,
    artifacts: Option<Vec<String>>,
}

impl RawBuilder {
    fn into_cfg(self) -> Result<BuilderCfg> {
        let source = self.source.ok_or_else(|| {
            anyhow::anyhow!("[builder] needs `source` — the host directory to mount at /src")
        })?;
        let script = self.script.ok_or_else(|| {
            anyhow::anyhow!(
                "[builder] needs `script` — the build commands run inside the builder VM"
            )
        })?;
        let artifacts = self.artifacts.unwrap_or_default();
        if artifacts.is_empty() {
            bail!("[builder] needs at least one `artifacts` entry — file names under /build/artifacts/");
        }
        if script.trim().is_empty() {
            bail!("[builder] script is empty");
        }
        Ok(BuilderCfg {
            image: self.image.unwrap_or_else(|| "debian:bookworm-slim".into()),
            source,
            tools: self.tools.unwrap_or_default(),
            script,
            artifacts,
        })
    }
}

/// A config value that accepts either a single string or a list of strings
/// (e.g. `entrypoint = "some-cli"` or `entrypoint = ["some-cli", "--flag"]`).
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

impl Loaded {
    /// The demo with defaults resolved; `name` is the machine-name stem.
    pub fn effective(&self) -> Result<EffectiveDemo> {
        let port = self.config.demo.port.ok_or_else(|| {
            anyhow::anyhow!(
                "no host port configured — set `port = 8410` (or similar) at the top of demo.toml"
            )
        })?;
        Ok(EffectiveDemo {
            // explicit `name` wins; otherwise the demo.toml directory name
            name: self
                .config
                .demo
                .name
                .clone()
                .map(|n| crate::util::ident(&n))
                .unwrap_or_else(|| self.slug.clone()),
            host_port: port,
            guest_port: self.config.defaults.guest_port,
            net: self.config.defaults.net,
            ssh_agent: self.config.defaults.ssh_agent,
            cpus: self.config.defaults.cpus,
            mem_mib: self.config.defaults.mem_mib,
            secret_env: self.config.defaults.secret_env.clone(),
            credentials: self
                .config
                .defaults
                .credentials
                .clone()
                .into_iter()
                .map(|(k, v)| (k, v.hosts))
                .collect(),
            include: self.config.demo.include.clone().unwrap_or_default(),
        })
    }
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
struct RawCredential {
    hosts: Option<Vec<String>>,
}

impl RawCredential {
    fn into_cfg(self, name: &str) -> Result<CredentialCfg> {
        let hosts = self.hosts.unwrap_or_default();
        if hosts.is_empty() {
            bail!(
                "credential `{name}` needs at least one host, e.g. `hosts = [\"api.example.com\"]`"
            );
        }
        Ok(CredentialCfg { hosts })
    }
}

pub fn default_packages() -> Vec<String> {
    ["ca-certificates", "curl", "git", "jq", "less", "ripgrep", "unzip"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl RawConfig {
    fn into_config(self) -> Result<Config> {
        let raw = self;
        let b = raw.build.unwrap_or_default();
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
        let builder = match raw.builder.map(RawBuilder::into_cfg) {
            Some(Ok(cfg)) => Some(cfg),
            Some(Err(e)) => bail!("{e}"),
            None => None,
        };
        let config = Config {
            build: BuildCfg {
                image: b.image.unwrap_or_else(|| "debian:bookworm-slim".into()),
                packages: b.packages.unwrap_or_else(default_packages),
                tools: b.tools.unwrap_or_default(),
                entrypoint: b.entrypoint.map(OneOrMany::into_vec).unwrap_or_default(),
                setup: b.setup,
                shell: b.shell.unwrap_or_else(|| "/bin/bash".into()),
                workspace: b.workspace.unwrap_or_else(|| "/workspace".into()),
            },
            builder,
            defaults: DefaultsCfg {
                net: d.net.unwrap_or(true),
                ssh_agent: d.ssh_agent.unwrap_or(true),
                secret_env: d.secret_env.unwrap_or_default(),
                credentials: defaults_credentials,
                guest_port: d.guest_port.unwrap_or(8080),
                cpus: d.cpus,
                mem_mib: d.mem,
            },
            demo: DemoCfg {
                name: raw.name,
                port: raw.port,
                include: raw.include,
            },
        };
        Ok(config)
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
                    "no demo.toml found in {} or any parent — run `stagehand init` or pass --config",
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
    let config: Config = raw.into_config()?;
    if let Some(b) = &config.builder {
        if !b.source.is_dir() {
            bail!("[builder] source directory not found: {}", b.source.display());
        }
    }

    let dot_dir = dir.join(".stagehand");
    let golden_path = dot_dir.join("golden.smolmachine");
    let rootfs_path = dot_dir.join("rootfs");
    let artifacts_dir = dot_dir.join("artifacts");
    let slug = slug(&dir);
    let template_name = format!("stagehand-template-{slug}");
    Ok(Loaded {
        config,
        dir,
        dot_dir,
        slug: slug.clone(),
        template_name,
        builder_name: format!("stagehand-builder-{slug}"),
        golden_path,
        rootfs_path,
        artifacts_dir,
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

pub const STARTER: &str = r#"# stagehand configuration — this file IS the demo.
# Full reference in the project README.

# Machine names derive from this. Drop it to use the directory name.
# name = "my-demo"

port = 8410                       # the demo is ALWAYS at http://localhost:8410
# include = ["/path/to/repo"]     # folders COPIED into the VM at `stagehand new`
#                                 # time — fresh copy every run, the host
#                                 # original is never mounted or modified

[defaults]
guest_port = 8080                 # port your webapp listens on inside the VM

# Credentials bound HOST-SIDE: the VM only ever sees a SMOLPLACEHOLDER_*
# value; smolvm substitutes the real value from your shell env on HTTPS
# requests to the hosts you list. The secret itself never enters the VM.
# List EVERY host the credential is sent to, matching what the client
# really connects to. The var must be set in your shell when a machine
# is STARTED.
#
# [defaults.credentials.MY_API_KEY]
# hosts = ["api.example.com"]

# For secrets that must be plainly visible inside the VM (non-HTTPS use):
# secret_env = ["SOME_PLAIN_TOKEN"]
# net = true                      # outbound networking (default true)
# ssh_agent = true                # forward your SSH agent for git pushes (default true)

[build]
image = "debian:bookworm-slim"    # base image for the golden image

# What `stagehand new` lands in. Drop the line for a plain shell.
# entrypoint = "some-cli"

# mise tools installed into the image; every binary they provide lands on PATH.
# tools = ["node@lts", "some-cli@latest"]

# Distro packages installed into the image.
# packages = ["git", "curl"]
guest_port = 8080                 # port your webapp listens on inside the VM

# Credentials bound HOST-SIDE: the VM only ever sees a SMOLPLACEHOLDER_*
# value; smolvm substitutes the real value from your shell env on HTTPS
# requests to the hosts you list. The secret itself never enters the VM.
# List EVERY host the credential is sent to, matching what the client
# really connects to. The var must be set in your shell when a machine
# is STARTED.
#
# [defaults.credentials.MY_API_KEY]
# hosts = ["api.example.com"]

# For secrets that must be plainly visible inside the VM (non-HTTPS use):
# secret_env = ["SOME_PLAIN_TOKEN"]
# net = true                      # outbound networking (default true)
# ssh_agent = true                # forward your SSH agent for git pushes (default true)

[demos.my-demo]
port = 8410                       # the demo is ALWAYS at http://localhost:8410

# [demos.another-demo]
# port = 8420
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_clean() {
        assert_eq!(slug(Path::new("/a/b/My Project")), "my-project");
        assert_eq!(slug(Path::new("/myproj")), "myproj");
        assert_eq!(slug(Path::new("/")), "default");
    }

    #[test]
    fn parses_starter() {
        let raw: RawConfig = toml::from_str(STARTER).unwrap();
        let cfg: Config = raw.into_config().unwrap();
        assert_eq!(cfg.demo.port, Some(8410));
        assert!(cfg.demo.include.is_none());
        assert!(cfg.defaults.credentials.is_empty());
    }

    #[test]
    fn parses_credentials() {
        let dir = std::env::temp_dir().join(format!("stagehand-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.toml");
        std::fs::write(&path, "port = 9000\n[defaults.credentials.MY_API_KEY]\nhosts = [\"api.example.com\"]\n").unwrap();
        let loaded = load(Some(&path)).unwrap();
        let eff = loaded.effective().unwrap();
        assert_eq!(
            eff.credentials.get("MY_API_KEY").map(Vec::as_slice),
            Some(["api.example.com".to_string()].as_slice())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn credential_without_hosts_is_rejected() {
        let raw: RawConfig = toml::from_str("[defaults.credentials.X]\n").unwrap();
        let cfg: Config = raw.into_config().unwrap();
        assert!(cfg.defaults.credentials.is_empty());
    }

    #[test]
    fn entrypoint_string_or_array_or_default() {
        let dir = std::env::temp_dir().join(format!("stagehand-ep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |toml_text: &str| {
            let path = dir.join("demo.toml");
            std::fs::write(&path, toml_text).unwrap();
            load(Some(&path)).unwrap()
        };

        // string form
        let loaded = mk("[build]\nentrypoint = \"some-cli\"\n[demos.x]\nport=1\n");
        assert_eq!(
            loaded.config.build.entrypoint_programs(),
            vec!["some-cli".to_string()]
        );

        // array form (program + args)
        let loaded = mk("[build]\nentrypoint = [\"some-cli\", \"--flag\"]\n[demos.x]\nport=1\n");
        assert_eq!(
            loaded.config.build.entrypoint_programs(),
            vec!["some-cli".to_string(), "--flag".to_string()]
        );

        // absent → shell
        let loaded = mk("port = 1\n");
        assert_eq!(
            loaded.config.build.entrypoint_programs(),
            vec!["/bin/bash".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

}
