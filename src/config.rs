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
/// Label marking the Linux build machine.
pub const ROLE_BUILDER: &str = "builder";

/// The golden image recipe (`[build]`): assembled by `democtl build`,
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
    /// What `democtl new`/`open` land in when no command override is given
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
    /// Demo used when a command is given without an explicit name.
    pub demo: Option<String>,
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

/// Optional Linux artifact stage (`[builder]`): a separate machine that
/// cross-compiles artifacts (e.g. from a host checkout mounted at `/src`)
/// which `democtl build` installs into the golden image.
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
    build: Option<RawBuild>,
    builder: Option<RawBuilder>,
    defaults: Option<RawDefaults>,
    #[serde(default)]
    demos: BTreeMap<String, RawDemo>,
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
    pub fn state_path(&self) -> PathBuf {
        self.dot_dir.join("state.json")
    }

    /// The demo last touched by an unnamed-capable command, if still configured.
    pub fn read_last_demo(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.state_path()).ok()?;
        let state: State = serde_json::from_str(&raw).ok()?;
        state.last_demo
    }

    /// Remember the demo for future unnamed commands. Failure to persist is
    /// non-fatal (the command already succeeded).
    pub fn write_last_demo(&self, demo: &str) {
        if let Err(e) = std::fs::create_dir_all(&self.dot_dir) {
            eprintln!("warning: could not create {}: {e}", self.dot_dir.display());
            return;
        }
        let state = State { last_demo: Some(demo.to_string()) };
        if let Err(e) = std::fs::write(self.state_path(), serde_json::to_string(&state).unwrap_or_default()) {
            eprintln!("warning: could not update {}: {e}", self.state_path().display());
        }
    }

    /// Which demo a command means when the name is omitted:
    /// explicit name → `[defaults] demo` pin → last used → the only demo.
    pub fn resolve_demo(&self, explicit: Option<&str>) -> Result<String> {
        if let Some(name) = explicit {
            self.config.demo(name)?;
            return Ok(name.to_string());
        }
        if let Some(pinned) = &self.config.defaults.demo {
            if self.config.demos.contains_key(pinned) {
                return Ok(pinned.clone());
            }
            bail!(
                "[defaults] demo = \"{pinned}\" is not a configured demo — configured: {}",
                self.demo_names()
            );
        }
        if let Some(last) = self.read_last_demo() {
            if self.config.demos.contains_key(&last) {
                return Ok(last);
            }
        }
        if self.config.demos.len() == 1 {
            return Ok(self.config.demos.keys().next().unwrap().clone());
        }
        bail!(
            "multiple demos configured ({}) and no default — pass one (democtl new <name>) or set [defaults] demo = \"...\"",
            self.demo_names()
        );
    }

    fn demo_names(&self) -> String {
        if self.config.demos.is_empty() {
            "(none)".to_string()
        } else {
            self.config.demos.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct State {
    last_demo: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct RawDefaults {
    demo: Option<String>,
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
        let mut config = Config {
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
                demo: d.demo,
                net: d.net.unwrap_or(true),
                ssh_agent: d.ssh_agent.unwrap_or(true),
                secret_env: d.secret_env.unwrap_or_default(),
                credentials: defaults_credentials,
                guest_port: d.guest_port.unwrap_or(8080),
                cpus: d.cpus,
                mem_mib: d.mem,
            },
            demos: BTreeMap::new(),
        };
        for (k, v) in raw.demos {
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
            config.demos.insert(
                k,
                DemoCfg {
                    port: v.port,
                    guest_port: v.guest_port,
                    cpus: v.cpus,
                    mem_mib: v.mem,
                    secret_env: v.secret_env,
                    credentials,
                },
            );
        }
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
    let config: Config = raw.into_config()?;
    if let Some(b) = &config.builder {
        if !b.source.is_dir() {
            bail!("[builder] source directory not found: {}", b.source.display());
        }
    }

    let dot_dir = dir.join(".democtl");
    let golden_path = dot_dir.join("golden.smolmachine");
    let rootfs_path = dot_dir.join("rootfs");
    let artifacts_dir = dot_dir.join("artifacts");
    let slug = slug(&dir);
    let template_name = format!("democtl-template-{slug}");
    Ok(Loaded {
        config,
        dot_dir,
        template_name,
        builder_name: format!("democtl-builder-{slug}"),
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

pub const STARTER: &str = r#"# democtl configuration — one section per conference demo.
# Full reference in the project README.

[build]
image = "debian:bookworm-slim"    # base image for the golden image

# What `democtl new` lands in. Drop the line for a plain shell.
# entrypoint = "some-cli"

# mise tools installed into the image; every binary they provide lands on PATH.
# tools = ["node@lts", "some-cli@latest"]

# Distro packages installed into the image.
# packages = ["git", "curl"]

# setup = """...'''..."""          # extra setup script, runs in the template

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
        assert_eq!(cfg.demos.len(), 1);
        let eff = cfg.effective("my-demo").unwrap();
        assert_eq!(eff.host_port, 8410);
        assert_eq!(eff.guest_port, 8080);
        assert!(eff.net);
    }

    #[test]
    fn missing_port_is_an_error() {
        let raw: RawConfig = toml::from_str("[demos.x]\n").unwrap();
        let cfg: Config = raw.into_config().unwrap();
        assert!(cfg.effective("x").is_err());
    }

    #[test]
    fn parses_credentials() {
        let raw: RawConfig = toml::from_str(
            "[defaults.credentials.MY_API_KEY]\nhosts = [\"api.example.com\"]\n\n[demos.y]\nport = 9000\n",
        )
        .unwrap();
        let cfg: Config = raw.into_config().unwrap();
        let eff = cfg.effective("y").unwrap();
        assert_eq!(
            eff.credentials.get("MY_API_KEY").map(Vec::as_slice),
            Some(["api.example.com".to_string()].as_slice())
        );
        // per-demo credentials replace defaults
        let raw: RawConfig = toml::from_str(
            "[defaults.credentials.A]\nhosts = [\"a.example\"]\n\n[demos.y.credentials.B]\nhosts = [\"b.example\"]\n\n[demos.y]\nport = 9000\n",
        )
        .unwrap();
        let cfg: Config = raw.into_config().unwrap();
        let eff = cfg.effective("y").unwrap();
        assert!(eff.credentials.contains_key("B"));
        assert!(!eff.credentials.contains_key("A"));
    }

    #[test]
    fn credential_without_hosts_is_rejected() {
        let raw: RawConfig = toml::from_str("[defaults.credentials.X]\n").unwrap();
        let cfg: Config = raw.into_config().unwrap();
        assert!(cfg.defaults.credentials.is_empty());
    }

    #[test]
    fn entrypoint_string_or_array_or_default() {
        let dir = std::env::temp_dir().join(format!("democtl-ep-{}", std::process::id()));
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
        let loaded = mk("[demos.x]\nport=1\n");
        assert_eq!(
            loaded.config.build.entrypoint_programs(),
            vec!["/bin/bash".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_demo_prefers_explicit_then_pin_then_last_used_then_single() {
        let dir = std::env::temp_dir().join(format!("democtl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |toml_text: &str| {
            let path = dir.join("demo.toml");
            std::fs::write(&path, toml_text).unwrap();
            load(Some(&path)).unwrap()
        };

        // explicit name always wins
        let loaded = mk("[demos.a]\nport=1\n[demos.b]\nport=2\n");
        assert_eq!(loaded.resolve_demo(Some("b")).unwrap(), "b");

        // ambiguous without pin/last-used
        assert!(loaded.resolve_demo(None).is_err());

        // last used remembered and honored
        loaded.write_last_demo("a");
        assert_eq!(loaded.resolve_demo(None).unwrap(), "a");

        // pin beats last used
        let loaded = mk("[defaults]\ndemo = \"b\"\n[demos.a]\nport=1\n[demos.b]\nport=2\n");
        assert_eq!(loaded.resolve_demo(None).unwrap(), "b");

        // single demo needs nothing
        let loaded = mk("[demos.only]\nport=1\n");
        assert_eq!(loaded.resolve_demo(None).unwrap(), "only");

        // stale last-used is ignored
        let loaded = mk("[demos.only]\nport=1\n");
        loaded.write_last_demo("deleted");
        assert_eq!(loaded.resolve_demo(None).unwrap(), "only");

        std::fs::remove_dir_all(&dir).ok();
    }
}
