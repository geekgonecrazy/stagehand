use crate::config::{
    LABEL_DEMO, LABEL_IDX, LABEL_MANAGED, LABEL_ROLE, ROLE_BUILDER, ROLE_TEMPLATE,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Thin wrapper over the `smolvm` CLI.
#[derive(Clone, Debug)]
pub struct Smol {
    bin: String,
}

impl Smol {
    pub fn new() -> Result<Self> {
        let bin = std::env::var("SMOLVM_BIN").unwrap_or_else(|_| "smolvm".to_string());
        let ok = Command::new(&bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            bail!("`{bin}` is not runnable — is smolvm installed and on PATH? (override with SMOLVM_BIN)");
        }
        Ok(Self { bin })
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args);
        c
    }

    /// Run smolvm, capture both streams (for JSON parsing robustness).
    pub fn capture_both(&self, args: &[&str]) -> Result<String> {
        let out = self
            .cmd(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("running smolvm {args:?}"))?;
        self.finish(args, out)
    }

    /// Run smolvm with inherited stdio (live progress / interactive), fail on error.
    pub fn run_inherit(&self, args: &[&str]) -> Result<()> {
        let status = self
            .cmd(args)
            .status()
            .with_context(|| format!("running smolvm {args:?}"))?;
        if !status.success() {
            bail!("smolvm {} failed", args.first().unwrap_or(&""));
        }
        Ok(())
    }

    /// Run smolvm with piped stdin (for scripts fed to `sh -s`) and
    /// inherited output, fail on non-zero exit.
    pub fn run_with_stdin(&self, args: &[&str], input: &str) -> Result<()> {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("running smolvm {args:?}"))?;
        child
            .stdin
            .take()
            .context("smolvm stdin")?
            .write_all(input.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            bail!("smolvm {} failed", args.first().unwrap_or(&""));
        }
        Ok(())
    }

    fn finish(&self, args: &[&str], out: std::process::Output) -> Result<String> {
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            bail!(
                "smolvm {} failed{}",
                args.first().unwrap_or(&""),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(":\n{stderr}")
                }
            );
        }
        Ok(stdout)
    }

    pub fn machines(&self) -> Result<Vec<Machine>> {
        let out = self.capture_both(&["machine", "ls", "--json"])?;
        if out.trim().is_empty() {
            return Ok(vec![]);
        }
        serde_json::from_str(&out).context("parsing `smolvm machine ls --json` output")
    }

    pub fn status(&self, name: &str) -> Result<Option<Machine>> {
        Ok(self.machines()?.into_iter().find(|m| m.name == name))
    }

    /// Every democtl-managed machine.
    pub fn managed(&self) -> Result<Vec<Machine>> {
        Ok(self.machines()?.into_iter().filter(|m| m.is_managed()).collect())
    }

    /// All managed machines of one demo, oldest first.
    pub fn managed_for_demo(&self, demo: &str) -> Result<Vec<Machine>> {
        let mut v: Vec<Machine> = self
            .managed()?
            .into_iter()
            .filter(|m| m.demo() == Some(demo))
            .collect();
        v.sort_by_key(|m| (m.idx(), m.created_at));
        Ok(v)
    }

    /// The machine a demo command should operate on (highest index).
    pub fn newest_for_demo(&self, demo: &str) -> Result<Option<Machine>> {
        Ok(self.managed_for_demo(demo)?.pop())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Machine {
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub created_at: i64,
}

impl Machine {
    pub fn is_managed(&self) -> bool {
        self.labels.get(LABEL_MANAGED).map(String::as_str) == Some("1")
    }

    pub fn demo(&self) -> Option<&str> {
        self.labels.get(LABEL_DEMO).map(String::as_str)
    }

    pub fn is_template(&self) -> bool {
        self.labels.get(LABEL_ROLE).map(String::as_str) == Some(ROLE_TEMPLATE)
    }

    pub fn is_builder(&self) -> bool {
        self.labels.get(LABEL_ROLE).map(String::as_str) == Some(ROLE_BUILDER)
    }

    /// Creation index from the `idx` label, falling back to the trailing
    /// number in the machine name (`democtl-checkout-3` → 3).
    pub fn idx(&self) -> Option<i64> {
        if let Some(i) = self.labels.get(LABEL_IDX).and_then(|v| v.parse().ok()) {
            return Some(i);
        }
        self.name.rsplit('-').next()?.parse().ok()
    }

    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// Build the `machine exec` argument list for an interactive, detached, or
/// one-shot command, including per-exec secret forwarding.
pub fn exec_args(
    name: &str,
    workdir: &str,
    interactive: bool,
    detach: bool,
    secrets: &[String],
    cmd: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = vec!["machine".into(), "exec".into()];
    if detach {
        args.push("-d".into());
    } else if interactive {
        args.push("-i".into());
        args.push("-t".into());
    }
    args.push("-w".into());
    args.push(workdir.into());
    args.push("--name".into());
    args.push(name.into());
    for s in secrets {
        args.push("--secret-env".into());
        args.push(s.clone());
    }
    args.push("--".into());
    args.extend(cmd.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str, labels: &[(&str, &str)]) -> Machine {
        Machine {
            name: name.into(),
            state: "stopped".into(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            created_at: 0,
        }
    }

    #[test]
    fn parses_ls_json() {
        let json = r#"[
          {"name":"democtl-checkout-1","state":"running","labels":{"democtl":"1","demo":"checkout","idx":"1"},"created_at":1791492903},
          {"name":"unrelated","state":"stopped","labels":{},"created_at":1}
        ]"#;
        let ms: Vec<Machine> = serde_json::from_str(json).unwrap();
        assert_eq!(ms.len(), 2);
        assert!(ms[0].is_managed());
        assert_eq!(ms[0].demo(), Some("checkout"));
        assert!(!ms[1].is_managed());
    }

    #[test]
    fn idx_falls_back_to_name() {
        let m = m("democtl-search-12", &[(LABEL_MANAGED, "1")]);
        assert_eq!(m.idx(), Some(12));
    }

    #[test]
    fn exec_args_shape() {
        let args = exec_args("vm", "/workspace", true, false, &["K=K".into()], &["opencode".into()]);
        assert_eq!(
            args,
            vec!["machine", "exec", "-i", "-t", "-w", "/workspace", "--name", "vm", "--secret-env", "K=K", "--", "opencode"]
        );
        let detached = exec_args("vm", "/workspace", false, true, &[], &["serve".into()]);
        assert_eq!(
            detached,
            vec!["machine", "exec", "-d", "-w", "/workspace", "--name", "vm", "--", "serve"]
        );
    }
}
