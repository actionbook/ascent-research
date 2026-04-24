use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

struct DoctorEnv {
    _tmp: TempDir,
    home: PathBuf,
    bin_dir: PathBuf,
}

impl DoctorEnv {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("research-home");
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        Self {
            _tmp: tmp,
            home,
            bin_dir,
        }
    }

    fn fake_bin(&self, name: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn fake_script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn run_with_bins(&self, postagent: &Path, actionbook: &Path) -> (Value, i32, String) {
        let out = Command::new(research_bin())
            .args(["--json", "doctor"])
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .env("POSTAGENT_BIN", postagent)
            .env("ACTIONBOOK_BIN", actionbook)
            .output()
            .expect("spawn ascent-research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout
            .lines()
            .find(|line| line.trim_start().starts_with('{'));
        let value = json_line
            .map(|line| serde_json::from_str(line).unwrap())
            .unwrap_or(Value::Null);
        (value, out.status.code().unwrap_or(-1), stderr)
    }

    fn run_args_with_bins(
        &self,
        args: &[&str],
        postagent: &Path,
        actionbook: &Path,
    ) -> (Value, i32, String) {
        let out = Command::new(research_bin())
            .args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .env("POSTAGENT_BIN", postagent)
            .env("ACTIONBOOK_BIN", actionbook)
            .output()
            .expect("spawn ascent-research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout
            .lines()
            .find(|line| line.trim_start().starts_with('{'));
        let value = json_line
            .map(|line| serde_json::from_str(line).unwrap())
            .unwrap_or(Value::Null);
        (value, out.status.code().unwrap_or(-1), stderr)
    }
}

fn checks_by_name(checks: &[Value]) -> HashMap<String, Value> {
    checks
        .iter()
        .map(|check| (check["name"].as_str().unwrap().to_string(), check.clone()))
        .collect()
}

fn contains_file_named(root: &Path, filename: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(filename) {
            return true;
        }
        if path.is_dir() && contains_file_named(&path, filename) {
            return true;
        }
    }
    false
}

#[test]
fn doctor_happy_path_with_fake_bins() {
    let env = DoctorEnv::new();
    let postagent = env.fake_bin("postagent");
    let actionbook = env.fake_bin("actionbook");

    let (v, code, stderr) = env.run_with_bins(&postagent, &actionbook);
    assert_eq!(code, 0, "stderr={stderr} envelope={v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "research doctor");
    assert_eq!(v["data"]["status"], "ok");
    assert_eq!(v["data"]["data_home"], env.home.to_string_lossy().as_ref());

    let checks = v["data"]["checks"].as_array().unwrap();
    let by_name = checks_by_name(checks);
    for name in [
        "data_home_writable",
        "builtin_preset_tech",
        "builtin_preset_sports",
        "postagent_bin",
        "actionbook_bin",
        "autoresearch_enabled",
    ] {
        let check = by_name
            .get(name)
            .unwrap_or_else(|| panic!("missing check {name}"));
        assert_eq!(check["required"], true, "{name} should be required");
        assert_eq!(check["ok"], true, "{name} should pass: {check}");
    }
}

#[test]
fn doctor_missing_required_dependencies_fails() {
    let env = DoctorEnv::new();
    let missing_postagent = env.bin_dir.join("missing-postagent");
    let missing_actionbook = env.bin_dir.join("missing-actionbook");

    let (v, code, _) = env.run_with_bins(&missing_postagent, &missing_actionbook);
    assert_ne!(code, 0, "doctor should fail when required deps are missing");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "DOCTOR_FAILED");
    assert_eq!(v["error"]["details"]["status"], "missing_required");
    assert!(
        v["error"]["details"]["install_hint"]
            .as_str()
            .unwrap()
            .contains("npm install -g postagent @actionbookdev/cli")
    );

    let checks = v["error"]["details"]["checks"].as_array().unwrap();
    let by_name = checks_by_name(checks);
    assert_eq!(by_name["postagent_bin"]["ok"], false);
    assert_eq!(by_name["actionbook_bin"]["ok"], false);
}

#[test]
fn doctor_provider_claude_disabled_is_optional() {
    let env = DoctorEnv::new();
    let postagent = env.fake_bin("postagent");
    let actionbook = env.fake_bin("actionbook");

    let (v, code, stderr) = env.run_with_bins(&postagent, &actionbook);
    assert_eq!(code, 0, "stderr={stderr} envelope={v}");
    let checks = v["data"]["checks"].as_array().unwrap();
    let by_name = checks_by_name(checks);
    let provider = &by_name["provider_claude_enabled"];
    assert_eq!(provider["required"], false);
    assert!(provider["ok"].is_boolean());
}

#[test]
fn doctor_provider_smoke_invalid_provider_fails_fast() {
    let env = DoctorEnv::new();
    let postagent = env.fake_bin("postagent");
    let actionbook = env.fake_bin("actionbook");

    let (v, code, _) = env.run_args_with_bins(
        &[
            "--json",
            "doctor",
            "--provider-smoke",
            "--provider",
            "unknown",
        ],
        &postagent,
        &actionbook,
    );
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_PROVIDER");
}

#[test]
fn doctor_tool_smoke_surfaces_optional_postagent_public_dry_run_failure() {
    let env = DoctorEnv::new();
    let postagent = env.fake_script(
        "postagent",
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "postagent 0.3.1"; exit 0; fi
if [ "$1" = "send" ] && [ "$2" = "--help" ]; then echo "send help"; exit 0; fi
if [ "$1" = "send" ] && [ "$3" = "--dry-run" ]; then echo "Missing token" >&2; exit 1; fi
exit 0
"#,
    );
    let actionbook = env.fake_script(
        "actionbook",
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "actionbook 1.6.0"; exit 0; fi
if [ "$1" = "browser" ] && [ "$2" = "list-sessions" ]; then echo '{"ok":true}'; exit 0; fi
exit 0
"#,
    );

    let (v, code, stderr) = env.run_args_with_bins(
        &["--json", "doctor", "--tool-smoke"],
        &postagent,
        &actionbook,
    );
    assert_eq!(
        code, 0,
        "optional dry-run failure must not fail doctor: stderr={stderr}; v={v}"
    );
    let checks = v["data"]["checks"].as_array().unwrap();
    let by_name = checks_by_name(checks);
    assert_eq!(by_name["postagent_version"]["ok"], true);
    assert_eq!(by_name["postagent_send_help"]["ok"], true);
    assert_eq!(by_name["postagent_public_dry_run"]["ok"], false);
    assert_eq!(by_name["postagent_public_dry_run"]["required"], false);
    assert_eq!(by_name["actionbook_browser_list_sessions"]["ok"], true);
}

#[test]
fn doctor_does_not_create_session() {
    let env = DoctorEnv::new();
    let postagent = env.fake_bin("postagent");
    let actionbook = env.fake_bin("actionbook");

    let (v, code, stderr) = env.run_with_bins(&postagent, &actionbook);
    assert_eq!(code, 0, "stderr={stderr} envelope={v}");
    assert!(!contains_file_named(&env.home, "session.jsonl"));
    assert!(!env.home.join(".active").exists());
}

#[test]
fn skill_recommends_doctor_before_playbooks() {
    let skill_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ascent-research/SKILL.md");
    let skill = fs::read_to_string(skill_path).unwrap();
    assert!(
        skill.contains("ascent-research --json doctor"),
        "skill must instruct agents to run doctor first"
    );
    assert!(
        skill.contains("STOP"),
        "skill must tell agents to stop when doctor fails"
    );
}
