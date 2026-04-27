use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

struct Env {
    _tmp: TempDir,
    home: String,
    bin_dir: PathBuf,
    postagent_log: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let bin_dir = tmp.path().join("_bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let postagent_log = tmp.path().join("postagent-requests.log");
        Self {
            _tmp: tmp,
            home,
            bin_dir,
            postagent_log,
        }
    }

    fn write_fake_bin(&self, name: &str, script: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn research(&self, args: &[&str]) -> (Value, String, String, i32) {
        self.research_with_postagent(args, None)
    }

    fn research_with_postagent(
        &self,
        args: &[&str],
        postagent: Option<&PathBuf>,
    ) -> (Value, String, String, i32) {
        self.research_with_postagent_env(args, postagent, &[])
    }

    fn research_with_postagent_env(
        &self,
        args: &[&str],
        postagent: Option<&PathBuf>,
        envs: &[(&str, &str)],
    ) -> (Value, String, String, i32) {
        let mut cmd = Command::new(binary());
        cmd.args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .env("POSTAGENT_REQUEST_LOG", &self.postagent_log);
        if let Some(postagent) = postagent {
            cmd.env("POSTAGENT_BIN", postagent);
        }
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn research binary");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(line) => serde_json::from_str(line).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, stdout, stderr, out.status.code().unwrap_or(-1))
    }

    fn postagent_log(&self) -> String {
        fs::read_to_string(&self.postagent_log).unwrap_or_default()
    }

    fn path(&self, name: &str) -> PathBuf {
        self._tmp.path().join(name)
    }
}

fn fake_github_postagent() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/dagster-io/dagster/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/dagster-io/dagster/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/dagster-io/dagster/contributors"*)
    cat <<'JSON'
[{"login":"alice"},{"login":"bob"},{"login":"carol"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/subscribers"*)
    cat <<'JSON'
[{"login":"watcher1"},{"login":"watcher2"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":42}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":1}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/stats/contributors"*)
    cat <<'JSON'
[{"total":100,"author":{"login":"alice"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":1,"author":{"login":"owner"}}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster"*)
    cat <<'JSON'
{"name":"dagster","full_name":"dagster-io/dagster","owner":{"login":"dagster-io"},"html_url":"https://github.com/dagster-io/dagster","stargazers_count":12345,"forks_count":2100,"open_issues_count":321,"watchers_count":12345}
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1,"watchers_count":10}
JSON
    exit 0 ;;
esac

printf '%s\n' "⚠ 404 — endpoint does not exist at $2" >&2
printf '%s\n' 'HTTP 404 Not Found' >&2
exit 0
"#
    .to_string()
}

fn fake_github_postagent_with_stargazers() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/dagster-io/dagster/stargazers?per_page=100&page=1"*)
    cat <<'JSON'
[{"starred_at":"2024-01-01T00:00:00Z","user":{"login":"u1"}},{"starred_at":"2024-02-01T00:00:00Z","user":{"login":"u2"}},{"login":"u3"}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/stargazers?per_page=100&page=2"*)
    printf '%s\n' 'page 2 should not be requested after short page' >&2
    exit 1 ;;
  *"/users/u1"*)
    cat <<'JSON'
{"login":"u1","created_at":"2023-01-01T00:00:00Z","followers":0,"public_repos":0,"bio":""}
JSON
    exit 0 ;;
  *"/users/u2"*)
    cat <<'JSON'
{"login":"u2","created_at":"2022-06-01T00:00:00Z","followers":1,"public_repos":2,"bio":null}
JSON
    exit 0 ;;
  *"/users/u3"*)
    cat <<'JSON'
{"login":"u3","created_at":"2020-01-01T00:00:00Z","followers":5,"public_repos":10,"bio":"builder"}
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/dagster-io/dagster/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/dagster-io/dagster/contributors"*)
    cat <<'JSON'
[{"login":"alice"},{"login":"bob"},{"login":"carol"}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/subscribers"*)
    cat <<'JSON'
[{"login":"watcher1"},{"login":"watcher2"}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":42}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster/stats/contributors"*)
    cat <<'JSON'
[{"total":100,"author":{"login":"alice"}}]
JSON
    exit 0 ;;
  *"/repos/dagster-io/dagster"*)
    cat <<'JSON'
{"name":"dagster","full_name":"dagster-io/dagster","owner":{"login":"dagster-io"},"html_url":"https://github.com/dagster-io/dagster","stargazers_count":12345,"forks_count":2100,"open_issues_count":321,"watchers_count":12345}
JSON
    exit 0 ;;
esac

printf '%s\n' "⚠ 404 — endpoint does not exist at $2" >&2
printf '%s\n' 'HTTP 404 Not Found' >&2
exit 0
"#
    .to_string()
}

fn fake_github_postagent_missing_github_credential() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *'$POSTAGENT.GITHUB.TOKEN'*)
    printf '%s\n' 'missing credential placeholder: $POSTAGENT.GITHUB.TOKEN' >&2
    exit 1 ;;
esac

printf '%s\n' "unexpected anonymous request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_timeline_burst() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stargazers?per_page=100&page=1"*)
    printf '['
    i=1
    while [ "$i" -le 100 ]; do
      if [ "$i" -gt 1 ]; then printf ','; fi
      if [ "$i" -le 60 ]; then
        starred_at='2024-03-01T12:00:00Z'
      else
        starred_at='2024-04-01T09:00:00Z'
      fi
      printf '{"starred_at":"%s","user":{"login":"u%s"}}' "$starred_at" "$i"
      i=$((i + 1))
    done
    printf ']\n'
    exit 0 ;;
  *"/users/u"*)
    login=$(printf '%s\n' "$*" | sed 's#.*https://api.github.com/users/\([^ ]*\).*#\1#')
    printf '{"login":"%s","created_at":"2024-02-01T00:00:00Z","followers":0,"public_repos":0,"bio":""}\n' "$login"
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":1}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":1,"author":{"login":"owner"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":100,"forks_count":1,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_organic_launch_burst() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stargazers?per_page=100&page=1"*)
    printf '['
    i=1
    while [ "$i" -le 100 ]; do
      if [ "$i" -gt 1 ]; then printf ','; fi
      if [ "$i" -le 75 ]; then
        starred_at='2026-04-01T12:00:00Z'
      else
        starred_at='2026-04-10T09:00:00Z'
      fi
      printf '{"starred_at":"%s","user":{"login":"u%s"}}' "$starred_at" "$i"
      i=$((i + 1))
    done
    printf ']\n'
    exit 0 ;;
  *"/users/u"*)
    login=$(printf '%s\n' "$*" | sed 's#.*https://api.github.com/users/\([^ ]*\).*#\1#')
    n=$(printf '%s\n' "$login" | sed 's#[^0-9]##g')
    month=$((n % 12 + 1))
    day=$((n % 27 + 1))
    printf '{"login":"%s","created_at":"2018-%02d-%02dT00:00:00Z","followers":8,"public_repos":6,"bio":"developer"}\n' "$login" "$month" "$day"
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"},{"login":"alice"},{"login":"bob"},{"login":"carol"},{"login":"dana"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher1"},{"login":"watcher2"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":12},{"week":1712448000,"total":9},{"week":1713052800,"total":7}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":20,"author":{"login":"owner"}},{"total":5,"author":{"login":"alice"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":100,"forks_count":12,"open_issues_count":3}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_uppercase_repo() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/Dagster-IO/Dagster/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/Dagster-IO/Dagster/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/Dagster-IO/Dagster/contributors"*)
    cat <<'JSON'
[{"login":"alice"}]
JSON
    exit 0 ;;
  *"/repos/Dagster-IO/Dagster/subscribers"*)
    cat <<'JSON'
[]
JSON
    exit 0 ;;
  *"/repos/Dagster-IO/Dagster/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":42}]
JSON
    exit 0 ;;
  *"/repos/Dagster-IO/Dagster/stats/contributors"*)
    cat <<'JSON'
[{"total":100,"author":{"login":"alice"}}]
JSON
    exit 0 ;;
  *"/repos/Dagster-IO/Dagster"*)
    cat <<'JSON'
{"name":"dagster","full_name":"dagster-io/dagster","owner":{"login":"dagster-io"},"html_url":"https://github.com/dagster-io/dagster","stargazers_count":12345,"forks_count":2100,"open_issues_count":321}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_empty_stargazer_page() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stargazers?per_page=100&page=1"*)
    cat <<'JSON'
[{"starred_at":"2024-01-01T00:00:00Z","user":{"login":"u1"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stargazers?per_page=100&page=2"*)
    printf '%s\n' 'page 2 should not be requested after short page' >&2
    exit 1 ;;
  *"/users/u1"*)
    cat <<'JSON'
{"login":"u1","created_at":"2023-01-01T00:00:00Z","followers":2,"public_repos":1,"bio":"ok"}
JSON
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":1}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":1,"author":{"login":"owner"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_repo_commit_risk() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":0},{"week":1712448000,"total":0},{"week":1713052800,"total":1}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":1,"author":{"login":"owner"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":5000,"forks_count":10,"open_issues_count":1,"watchers_count":5000}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_stats_202_and_traffic_429() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stats/commit_activity"*)
    printf '%s\n' '⚠ 202 — stats are being generated at https://api.github.com/repos/owner/repo/stats/commit_activity' >&2
    printf '%s\n' 'HTTP 202 Accepted' >&2
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    printf '%s\n' '⚠ 202 — stats are being generated at https://api.github.com/repos/owner/repo/stats/contributors' >&2
    printf '%s\n' 'HTTP 202 Accepted' >&2
    exit 0 ;;
  *"/repos/owner/repo/traffic/views"*)
    printf '%s\n' '⚠ 429 — rate limit exceeded at https://api.github.com/repos/owner/repo/traffic/views' >&2
    printf '%s\n' 'HTTP 429 Too Many Requests' >&2
    exit 0 ;;
  *"/repos/owner/repo/traffic/clones"*)
    cat <<'JSON'
{"count":10,"uniques":5,"clones":[]}
JSON
    exit 0 ;;
  *"/repos/owner/repo/traffic/popular/referrers"*)
    printf '%s\n' 'not-json'
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "⚠ 404 — endpoint does not exist at $2" >&2
printf '%s\n' 'HTTP 404 Not Found' >&2
exit 0
"#
    .to_string()
}

fn fake_github_postagent_stats_unavailable() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stats/commit_activity"*)
    printf '%s\n' '⚠ 500 — stats unavailable at https://api.github.com/repos/owner/repo/stats/commit_activity' >&2
    printf '%s\n' 'HTTP 500 Internal Server Error' >&2
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    printf '%s\n' '⚠ 403 — stats unavailable at https://api.github.com/repos/owner/repo/stats/contributors' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_contributor_stats_unavailable() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
[{"week":1711843200,"total":3},{"week":1712448000,"total":4},{"week":1713052800,"total":5}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    printf '%s\n' '⚠ 403 — stats unavailable at https://api.github.com/repos/owner/repo/stats/contributors' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "unexpected request: $*" >&2
exit 1
"#
    .to_string()
}

fn fake_github_postagent_malformed_repo() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":"many","forks_count":2}
JSON
    exit 0 ;;
esac

cat <<'JSON'
[]
JSON
exit 0
"#
    .to_string()
}

fn fake_github_postagent_pending_stats_body() -> String {
    r#"#!/bin/sh
if [ -n "$POSTAGENT_REQUEST_LOG" ]; then
  printf '%s\n' "$*" >> "$POSTAGENT_REQUEST_LOG"
fi

case "$*" in
  *"/repos/owner/repo/stats/commit_activity"*)
    cat <<'JSON'
{"message":"Statistics are being generated, try again later."}
JSON
    exit 0 ;;
  *"/repos/owner/repo/stats/contributors"*)
    cat <<'JSON'
[{"total":1,"author":{"login":"owner"}}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/contributors"*)
    cat <<'JSON'
[{"login":"owner"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/subscribers"*)
    cat <<'JSON'
[{"login":"watcher"}]
JSON
    exit 0 ;;
  *"/repos/owner/repo/traffic/"*)
    printf '%s\n' '⚠ 403 — endpoint requires authorization at https://api.github.com/repos/owner/repo/traffic' >&2
    printf '%s\n' 'HTTP 403 Forbidden' >&2
    exit 0 ;;
  *"/repos/owner/repo"*)
    cat <<'JSON'
{"name":"repo","full_name":"owner/repo","owner":{"login":"owner"},"html_url":"https://github.com/owner/repo","stargazers_count":10,"forks_count":2,"open_issues_count":1}
JSON
    exit 0 ;;
esac

printf '%s\n' "⚠ 404 — endpoint does not exist at $2" >&2
printf '%s\n' 'HTTP 404 Not Found' >&2
exit 0
"#
    .to_string()
}

fn array_contains_endpoint(items: &Value, suffix: &str) -> bool {
    items.as_array().unwrap().iter().any(|item| {
        item["endpoint"]
            .as_str()
            .or_else(|| item["path"].as_str())
            .is_some_and(|endpoint| endpoint.ends_with(suffix))
    })
}

fn endpoint_status(items: &Value, suffix: &str) -> Option<i64> {
    items.as_array().unwrap().iter().find_map(|item| {
        let endpoint = item["endpoint"]
            .as_str()
            .or_else(|| item["path"].as_str())?;
        endpoint
            .ends_with(suffix)
            .then(|| item["status"].as_i64())
            .flatten()
    })
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

#[test]
fn github_audit_repo_depth_anonymous_success() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent());

    let (v, stdout, _, code) = env.research_with_postagent(
        &[
            "--json",
            "github-audit",
            "dagster-io/dagster",
            "--depth",
            "repo",
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["depth"], "repo");
    assert_eq!(v["data"]["repository"]["owner"], "dagster-io");
    assert_eq!(v["data"]["repository"]["repo"], "dagster");
    assert_eq!(
        v["data"]["repository"]["html_url"],
        "https://github.com/dagster-io/dagster"
    );
    assert_eq!(v["data"]["repository"]["stars"], 12345);
    assert_eq!(v["data"]["signals"]["repo"]["contributors_count"], 3);
    assert_eq!(v["data"]["signals"]["repo"]["subscribers_count"], 2);
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "github_native_stats"
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["fork_star_ratio"],
        2100.0 / 12345.0
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["subscriber_star_ratio"],
        2.0 / 12345.0
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["issue_star_ratio"],
        321.0 / 12345.0
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["contributors_star_ratio"],
        3.0 / 12345.0
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_total_52w"],
        42
    );
    assert_eq!(v["data"]["signals"]["repo"]["watchers_count_ignored"], true);
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/traffic/views"
    ));
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/traffic/clones"
    ));
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/traffic/popular/referrers"
    ));
    let score = v["data"]["risk"]["score"].as_i64().unwrap();
    assert!((0..=100).contains(&score));
    assert!(!stdout.contains("Authorization"));
    assert!(!stdout.contains("GITHUB.TOKEN"));

    let log = env.postagent_log();
    assert!(log.contains("/repos/dagster-io/dagster"));
    assert!(log.contains("Accept: application/vnd.github+json"));
    assert!(!log.contains("Authorization"));
    assert!(!log.contains("GITHUB.TOKEN"));
}

#[test]
fn github_audit_prefers_native_github_stats() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_repo_commit_risk());

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "github_native_stats"
    );
    assert_eq!(v["data"]["signals"]["repo"]["commit_activity_total_52w"], 1);
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap()
                .starts_with("low_commit_activity_per_star="))
    );
    assert!(
        v["data"]["risk"]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "/repos/owner/repo/stats/commit_activity")
    );
}

#[test]
fn github_audit_does_not_treat_watchers_count_as_subscribers() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent());

    let (v, _, _, code) = env.research_with_postagent(
        &[
            "--json",
            "github-audit",
            "dagster-io/dagster",
            "--depth",
            "repo",
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(v["data"]["signals"]["repo"]["subscribers_count"], 2);
    assert_eq!(
        v["data"]["signals"]["repo"]["subscriber_star_ratio"],
        2.0 / 12345.0
    );
    assert_ne!(
        v["data"]["signals"]["repo"]["subscriber_star_ratio"],
        12345.0 / 12345.0
    );
}

#[test]
fn github_audit_accepts_github_url_input() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent());

    let (v, _, _, code) = env.research_with_postagent(
        &[
            "--json",
            "github-audit",
            "https://github.com/dagster-io/dagster",
            "--depth",
            "repo",
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(v["data"]["repository"]["owner"], "dagster-io");
    assert_eq!(v["data"]["repository"]["repo"], "dagster");
}

#[test]
fn github_audit_stargazers_requires_postagent_github_token() {
    let env = Env::new();
    let postagent = env.write_fake_bin(
        "postagent",
        &fake_github_postagent_missing_github_credential(),
    );

    let (v, stdout, stderr, code) = env.research_with_postagent_env(
        &[
            "--json",
            "github-audit",
            "dagster-io/dagster",
            "--depth",
            "stargazers",
        ],
        Some(&postagent),
        &[("POSTAGENT_GITHUB_TOKEN", "secret-token")],
    );

    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "GITHUB_TOKEN_REQUIRED");
    assert_eq!(v["error"]["details"]["depth"], "stargazers");
    assert_eq!(v["error"]["details"]["sub_code"], "GITHUB_TOKEN_REQUIRED");
    let log = env.postagent_log();
    assert!(log.contains("$POSTAGENT.GITHUB.TOKEN"));
    assert!(!stdout.contains("secret-token"));
    assert!(!stderr.contains("secret-token"));
    assert!(!log.contains("secret-token"));
}

#[test]
fn github_audit_default_depth_and_sample() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_with_stargazers());

    let (v, stdout, stderr, code) = env.research_with_postagent_env(
        &["--json", "github-audit", "dagster-io/dagster"],
        Some(&postagent),
        &[],
    );

    assert_eq!(code, 0, "{v:#?}\nstdout={stdout}\nstderr={stderr}");
    assert_eq!(v["data"]["depth"], "stargazers");
    assert_eq!(v["data"]["sample"]["requested"], 200);
    assert_eq!(v["data"]["sample"]["fetched"], 3);
    assert!(v["data"]["sample"]["pages"].as_u64().unwrap() >= 1);
    assert_eq!(v["data"]["github_api"]["authenticated"], true);
    assert_eq!(v["data"]["signals"]["stargazers"]["accounts_sampled"], 3);
    assert_eq!(
        v["data"]["signals"]["stargazers"]["empty_bio_share"],
        2.0 / 3.0
    );
    assert_eq!(
        v["data"]["signals"]["stargazers"]["zero_public_repos_share"],
        1.0 / 3.0
    );
    assert_eq!(
        v["data"]["signals"]["stargazers"]["low_follower_share"],
        2.0 / 3.0
    );
    assert_eq!(
        v["data"]["signals"]["stargazers"]["zero_follower_share"],
        1.0 / 3.0
    );

    let log = env.postagent_log();
    assert!(log.contains("application/vnd.github.star+json"));
    assert!(log.contains("application/vnd.github+json"));
    assert!(log.contains("$POSTAGENT.GITHUB.TOKEN"));
    assert!(!stdout.contains("Authorization"));
    assert!(!stderr.contains("Authorization"));
}

#[test]
fn github_audit_timeline_computes_burst_signals() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_timeline_burst());

    let (v, stdout, stderr, code) = env.research_with_postagent_env(
        &[
            "--json",
            "github-audit",
            "owner/repo",
            "--depth",
            "timeline",
            "--sample",
            "100",
        ],
        Some(&postagent),
        &[],
    );

    assert_eq!(code, 0, "{v:#?}\nstdout={stdout}\nstderr={stderr}");
    assert_eq!(v["data"]["depth"], "timeline");
    assert_eq!(v["data"]["sample"]["requested"], 100);
    assert_eq!(v["data"]["sample"]["fetched"], 100);
    assert_eq!(
        v["data"]["signals"]["timeline"]["starred_at_available_count"],
        100
    );
    assert!(
        v["data"]["signals"]["timeline"]["max_daily_star_share"]
            .as_f64()
            .unwrap()
            >= 0.60
    );
    assert!(
        v["data"]["signals"]["timeline"]["max_hourly_star_share"]
            .as_f64()
            .unwrap()
            >= 0.60
    );
    assert!(
        v["data"]["signals"]["timeline"]["max_24h_star_share"]
            .as_f64()
            .unwrap()
            >= 0.60
    );
    assert!(v["data"]["risk"]["score"].as_i64().unwrap() > 0);
    assert_ne!(v["data"]["risk"]["band"], "low");
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap()
                .starts_with("max_daily_star_share="))
    );
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "star_burst")
    );
    let log = env.postagent_log();
    assert!(log.contains("$POSTAGENT.GITHUB.TOKEN"));
    assert!(!stdout.contains("Authorization"));
    assert!(!stderr.contains("Authorization"));
}

#[test]
fn github_audit_organic_launch_burst_keeps_high_trust_score() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_organic_launch_burst());

    let (v, stdout, stderr, code) = env.research_with_postagent_env(
        &[
            "--json",
            "github-audit",
            "owner/repo",
            "--depth",
            "timeline",
            "--sample",
            "100",
        ],
        Some(&postagent),
        &[],
    );

    assert_eq!(code, 0, "{v:#?}\nstdout={stdout}\nstderr={stderr}");
    assert!(
        v["data"]["signals"]["timeline"]["max_24h_star_share"]
            .as_f64()
            .unwrap()
            >= 0.70
    );
    assert!(v["data"]["risk"]["score"].as_i64().unwrap() <= 10);
    assert_eq!(v["data"]["risk"]["band"], "low");
    assert!(v["data"]["trust"]["score"].as_i64().unwrap() >= 90);
    assert_eq!(v["data"]["trust"]["band"], "high");
}

#[test]
fn skill_recommends_github_audit_for_trust_reports() {
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/ascent-research/SKILL.md");
    let skill = std::fs::read_to_string(skill_path).unwrap();
    assert!(skill.contains("ascent-research github-audit"));
    assert!(skill.contains("--preset github-trust"));
    assert!(skill.contains("ascent-research finish"));
    assert!(skill.contains("risk score"));
}

#[test]
fn github_audit_out_writes_full_envelope() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent());
    let out_path = env.path("audit.json");
    let out_arg = out_path.to_string_lossy().into_owned();

    let (v, _, _, code) = env.research_with_postagent(
        &[
            "--json",
            "github-audit",
            "dagster-io/dagster",
            "--depth",
            "repo",
            "--out",
            &out_arg,
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    let written: Value = serde_json::from_str(&fs::read_to_string(out_path).unwrap()).unwrap();
    assert_eq!(written["ok"], true);
    assert_eq!(written["command"], "research github-audit");
    assert_eq!(written["data"]["depth"], "repo");
    assert_eq!(written["data"]["repository"]["owner"], "dagster-io");
    assert_eq!(written["data"]["risk"], v["data"]["risk"]);
    assert_eq!(written, v);
}

#[test]
fn github_audit_html_report_renders_trust_scorecard() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_timeline_burst());
    let html_path = env.path("audit.html");
    let html_arg = html_path.to_string_lossy().into_owned();

    let (v, stdout, stderr, code) = env.research_with_postagent_env(
        &[
            "--json",
            "github-audit",
            "owner/repo",
            "--depth",
            "timeline",
            "--sample",
            "100",
            "--html",
            &html_arg,
        ],
        Some(&postagent),
        &[],
    );

    assert_eq!(code, 0, "{v:#?}\nstdout={stdout}\nstderr={stderr}");
    assert_eq!(v["data"]["html_out"], html_arg);
    let html = fs::read_to_string(html_path).unwrap();
    assert!(html.contains("GitHub Trust Audit"));
    assert!(html.contains("owner/repo"));
    assert!(html.contains("Trust score"));
    assert!(html.contains("Risk score"));
    assert!(html.contains("Confidence"));
    assert!(html.contains("Evidence status"));
    assert!(html.contains("trust-score-value"));
    assert!(html.contains("risk-score-value"));
    assert!(html.contains("confidence-value"));
    assert!(html.contains("trust.score"));
    assert!(html.contains("max_24h_star_share"));
    assert!(html.contains("max_daily_star_share"));
    assert!(html.contains("empty_bio_share"));
    assert!(html.contains("subscriber_star_ratio"));
    assert!(html.contains("<svg"));
    assert!(html.contains("Not a fake/real verdict"));
    assert!(html.contains("partial evidence means some GitHub-native checks are incomplete"));
    assert!(!html.contains("Authorization"));
    assert!(!html.contains("GITHUB.TOKEN"));
}

#[test]
fn github_audit_human_output_is_summary_only() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent());
    let out_path = env.path("audit.json");
    let out_arg = out_path.to_string_lossy().into_owned();

    let (_, stdout, stderr, code) = env.research_with_postagent(
        &[
            "github-audit",
            "dagster-io/dagster",
            "--depth",
            "repo",
            "--out",
            &out_arg,
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("repo: dagster-io/dagster"));
    assert!(stdout.contains("depth: repo"));
    assert!(stdout.contains("risk:"));
    assert!(stdout.contains("out:"));
    assert!(!stdout.contains("\"signals\""));
    assert!(!stdout.contains("signals:"));
    assert!(!stdout.contains("github_api"));
    assert!(!stdout.contains("Authorization"));
    assert!(!stdout.contains("GITHUB.TOKEN"));
    assert!(!stdout.trim_start().starts_with('{'));
}

#[test]
fn github_audit_uppercase_input_accepts_canonical_repo_response() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_uppercase_repo());

    let (v, _, _, code) = env.research_with_postagent(
        &[
            "--json",
            "github-audit",
            "Dagster-IO/Dagster",
            "--depth",
            "repo",
        ],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(v["data"]["repository"]["owner"], "dagster-io");
    assert_eq!(v["data"]["repository"]["repo"], "dagster");
}

#[test]
fn github_audit_short_stargazer_page_stops_pagination() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_empty_stargazer_page());

    let (v, _, stderr, code) = env.research_with_postagent_env(
        &[
            "--json",
            "github-audit",
            "owner/repo",
            "--depth",
            "stargazers",
            "--sample",
            "300",
        ],
        Some(&postagent),
        &[],
    );

    assert_eq!(code, 0, "{v:#?}\nstderr={stderr}");
    assert_eq!(v["data"]["sample"]["requested"], 300);
    assert_eq!(v["data"]["sample"]["fetched"], 1);
    assert_eq!(v["data"]["sample"]["pages"], 1);
    let log = env.postagent_log();
    assert!(log.contains("page=1"));
    assert!(!log.contains("page=2"));
}

#[test]
fn github_audit_rejects_invalid_repo_inputs() {
    let env = Env::new();
    let invalid = [
        "",
        " ",
        "owner",
        "owner/repo/extra",
        "/owner/repo",
        "owner/repo/",
        "owner /repo",
        "owner/re po",
        "owner/repo?x=1",
        "owner/repo#frag",
        "owner-/repo",
        "own_er/repo",
        "owner/.. /repo",
        "https://github.com/owner/repo/extra",
        "https://github.com/owner/repo?x=1",
        "https://github.com/-owner/repo",
    ];

    for repo in invalid {
        let (v, _, _, code) = env.research(&["--json", "github-audit", repo, "--depth", "repo"]);
        assert_ne!(code, 0, "input should fail: {repo:?}");
        assert_eq!(
            v["error"]["code"], "INVALID_ARGUMENT",
            "input should be INVALID_ARGUMENT: {repo:?}"
        );
    }

    let (v, _, _, code) = env.research(&["--json", "github-audit", "--", "-owner/repo"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn github_audit_treats_stats_202_and_traffic_429_as_unavailable() {
    let env = Env::new();
    let postagent = env.write_fake_bin(
        "postagent",
        &fake_github_postagent_stats_202_and_traffic_429(),
    );

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "stats_pending"
    );
    assert_ne!(v["data"]["risk"]["band"], "unknown");
    assert_eq!(v["data"]["risk"]["evidence_status"], "partial");
    assert_eq!(v["data"]["risk"]["confidence"], 0.35);
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "github_stats_pending")
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/commit_activity"
    ));
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/contributors"
    ));
    assert_eq!(
        endpoint_status(&v["data"]["github_api"]["unavailable"], "/traffic/views"),
        Some(429)
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/traffic/popular/referrers"
    ));
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["endpoints"],
        "/traffic/clones"
    ));
    assert!(!array_contains_endpoint(
        &v["data"]["github_api"]["endpoints"],
        "/traffic/views"
    ));
}

#[test]
fn github_audit_stats_unavailable_returns_partial_evidence() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_stats_unavailable());

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "unavailable"
    );
    assert_ne!(v["data"]["risk"]["band"], "unknown");
    assert_eq!(v["data"]["risk"]["evidence_status"], "partial");
    assert_eq!(v["data"]["risk"]["confidence"], 0.35);
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "github_stats_unavailable")
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/commit_activity"
    ));
    assert_eq!(
        endpoint_status(
            &v["data"]["github_api"]["unavailable"],
            "/stats/commit_activity"
        ),
        Some(500)
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/contributors"
    ));
    assert_eq!(
        endpoint_status(
            &v["data"]["github_api"]["unavailable"],
            "/stats/contributors"
        ),
        Some(403)
    );
}

#[test]
fn github_audit_contributor_stats_unavailable_returns_partial_evidence() {
    let env = Env::new();
    let postagent = env.write_fake_bin(
        "postagent",
        &fake_github_postagent_contributor_stats_unavailable(),
    );

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_eq!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "github_native_stats"
    );
    assert_eq!(
        v["data"]["signals"]["repo"]["stats_contributors_source"],
        "unavailable"
    );
    assert_ne!(v["data"]["risk"]["band"], "unknown");
    assert_eq!(v["data"]["risk"]["evidence_status"], "partial");
    assert_eq!(v["data"]["risk"]["confidence"], 0.35);
    assert!(
        v["data"]["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "github_stats_unavailable")
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["endpoints"],
        "/stats/commit_activity"
    ));
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/contributors"
    ));
    assert_eq!(
        endpoint_status(
            &v["data"]["github_api"]["unavailable"],
            "/stats/contributors"
        ),
        Some(403)
    );
}

#[test]
fn github_audit_malformed_repo_json_fails() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_malformed_repo());

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "GITHUB_API_ERROR");
}

#[test]
fn github_audit_treats_stats_object_body_as_pending() {
    let env = Env::new();
    let postagent = env.write_fake_bin("postagent", &fake_github_postagent_pending_stats_body());

    let (v, _, _, code) = env.research_with_postagent(
        &["--json", "github-audit", "owner/repo", "--depth", "repo"],
        Some(&postagent),
    );

    assert_eq!(code, 0, "{v:#?}");
    assert_ne!(
        v["data"]["signals"]["repo"]["commit_activity_source"],
        "github_native_stats"
    );
    assert!(array_contains_endpoint(
        &v["data"]["github_api"]["unavailable"],
        "/stats/commit_activity"
    ));
}
