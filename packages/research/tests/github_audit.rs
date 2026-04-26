use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

struct Env {
    _tmp: TempDir,
    home: String,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        Self { _tmp: tmp, home }
    }

    fn research(&self, args: &[&str]) -> (Value, String, String, i32) {
        let out = Command::new(binary())
            .args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .output()
            .expect("spawn research binary");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(line) => serde_json::from_str(line).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, stdout, stderr, out.status.code().unwrap_or(-1))
    }
}

#[test]
fn github_audit_rejects_invalid_depth_and_sample() {
    let env = Env::new();
    let (v, _, _, code) =
        env.research(&["--json", "github-audit", "owner/repo", "--depth", "full"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");

    let (v, _, _, code) = env.research(&["--json", "github-audit", "owner/repo", "--sample", "0"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}
