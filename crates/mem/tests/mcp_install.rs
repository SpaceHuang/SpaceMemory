use assert_cmd::Command;
use serde_json::Value;

fn mem() -> Command {
    Command::cargo_bin("mem").expect("mem 二进制")
}

#[test]
fn root_and_mcp_help_describe_installation_without_starting_stdio_server() {
    let root = mem().arg("--help").output().unwrap();
    assert!(root.status.success());
    let root_text = String::from_utf8_lossy(&root.stdout);
    assert!(root_text.contains("mcp"), "{root_text}");

    let mcp_help = mem().args(["mcp", "--help"]).output().unwrap();
    assert!(mcp_help.status.success());
    let mcp_text = String::from_utf8_lossy(&mcp_help.stdout);
    assert!(mcp_text.contains("install"), "{mcp_text}");
    assert!(mcp_text.contains("Codex"), "{mcp_text}");
    assert!(mcp_text.contains("Claude Code"), "{mcp_text}");
}

#[test]
fn install_without_client_prints_host_neutral_stdio_instructions() {
    for args in [vec!["mcp", "install"], vec!["mcp", "install", "--client"]] {
        let out = mem().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let guide = String::from_utf8_lossy(&out.stdout);
        assert!(guide.contains("spacememory"), "{guide}");
        assert!(guide.contains("stdio"), "{guide}");
        assert!(guide.contains("[\"mcp\"]"), "{guide}");
        let marker = "启动命令（JSON 字符串）是 ";
        let line = guide.lines().find(|line| line.contains(marker)).unwrap();
        let encoded_path = line
            .split_once(marker)
            .unwrap()
            .1
            .split_once("；参数数组")
            .unwrap()
            .0
            .trim_matches('`');
        let executable: String =
            serde_json::from_str(encoded_path).expect("路径必须是有效 JSON 字符串");
        assert!(
            std::path::Path::new(&executable).is_absolute(),
            "{executable}"
        );
        assert!(
            std::path::Path::new(&executable)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mem"),
            "{executable}"
        );
        assert!(
            guide.contains("发现已有同名"),
            "必须明确遇到同名时停止：{guide}"
        );
        assert!(
            !guide.contains("claude mcp add"),
            "通用引导不猜宿主：{guide}"
        );
        assert!(
            !guide.contains("codex mcp add"),
            "通用引导不猜宿主：{guide}"
        );
    }
}

#[test]
fn generic_install_does_not_create_or_read_the_memory_store() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store-that-must-not-be-created");
    let out = mem()
        .args(["--store", store.to_str().unwrap(), "mcp", "install"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store.exists(), "安装引导不得初始化或触碰记忆店");
}

#[test]
fn unknown_client_name_fails_with_supported_choices() {
    let out = mem()
        .args(["mcp", "install", "--client", "other-agent"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("codex") && error.contains("claude-code"),
        "{error}"
    );
}

#[test]
fn misspelled_mcp_subcommand_is_an_error_not_an_empty_server_start() {
    let out = mem().args(["mcp", "instlal"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"));
}

#[cfg(unix)]
#[test]
fn installer_skips_non_executable_client_file_and_finds_later_executable() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second dir");
    let codex_home = temp.path().join("codex home");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();

    let not_executable = first.join("codex");
    std::fs::write(&not_executable, "not executable").unwrap();
    std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644)).unwrap();

    let cli = second.join("codex");
    std::fs::write(
        &cli,
        r#"#!/bin/sh
if [ "$1" != mcp ] || [ "$2" != add ] || [ "$3" != spacememory ] || [ "$4" != -- ] || [ "$5" != "$EXPECTED_MEM" ] || [ "$6" != mcp ]; then exit 31; fi
printf '[mcp_servers.spacememory]\ncommand = "%s"\nargs = ["mcp"]\n' "$EXPECTED_MEM" > "$CODEX_HOME/config.toml"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mem_source = assert_cmd::cargo::cargo_bin("mem");
    let mem_dir = temp.path().join("mem with spaces");
    std::fs::create_dir_all(&mem_dir).unwrap();
    let mem_exe = mem_dir.join("记忆 mem");
    std::fs::copy(mem_source, &mem_exe).unwrap();
    std::fs::set_permissions(&mem_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([first, second]).unwrap();
    let out = std::process::Command::new(&mem_exe)
        .args(["mcp", "install", "--client", "codex"])
        .env("PATH", path)
        .env("CODEX_HOME", &codex_home)
        .env("EXPECTED_MEM", &mem_exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(has_config_entry(&codex_home.join("config.toml"), &mem_exe));
}

#[cfg(unix)]
fn has_config_entry(config: &std::path::Path, mem: &std::path::Path) -> bool {
    let text = std::fs::read_to_string(config).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    parsed["mcp_servers"]["spacememory"]["command"].as_str() == mem.to_str()
        && parsed["mcp_servers"]["spacememory"]["args"][0].as_str() == Some("mcp")
}

#[cfg(windows)]
#[test]
fn installer_runs_npm_powershell_shim_under_restricted_policy_and_preserves_argv() {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("npm bin 空格 ' & (x)");
    let codex_home = temp.path().join("Codex 用户配置");
    let mem_dir = temp.path().join("mem 路径 ' & (x)");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::create_dir_all(&mem_dir).unwrap();

    let cli = bin.join("codex.ps1");
    let receipt = temp.path().join("received-arguments.json");
    let policy_receipt = temp.path().join("effective-policy.txt");
    let config = codex_home.join("config.toml");
    let script = format!(
        r####"$ErrorActionPreference = 'Stop'
$received = @($args)
[System.IO.File]::WriteAllText('{receipt}', (ConvertTo-Json -InputObject $received -Compress), (New-Object System.Text.UTF8Encoding $false))
[System.IO.File]::WriteAllText('{policy_receipt}', (Get-ExecutionPolicy), (New-Object System.Text.UTF8Encoding $false))
if ($received.Count -ne 6 -or $received[0] -ne 'mcp' -or $received[1] -ne 'add' -or $received[2] -ne 'spacememory' -or $received[3] -ne '--' -or $received[5] -ne 'mcp') {{ exit 31 }}
$command = $received[4].Replace('\', '\\').Replace('"', '\"')
$toml = "[mcp_servers.spacememory]`ncommand = `"$command`"`nargs = [`"mcp`"]`n"
[System.IO.File]::WriteAllText('{config}', $toml, (New-Object System.Text.UTF8Encoding $false))
"####,
        receipt = ps_literal(&receipt),
        policy_receipt = ps_literal(&policy_receipt),
        config = ps_literal(&config),
    );
    let mut script_bytes = vec![0xEF, 0xBB, 0xBF];
    script_bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&cli, script_bytes).unwrap();

    let mem_source = assert_cmd::cargo::cargo_bin("mem");
    let mem_exe = mem_dir.join("记忆 mem.exe");
    std::fs::copy(mem_source, &mem_exe).unwrap();
    let path = std::env::join_paths([&bin]).unwrap();
    let mut cmd = std::process::Command::new(&mem_exe);
    cmd.args(["mcp", "install", "--client", "codex"])
        .env("PATH", path)
        .env("CODEX_HOME", &codex_home)
        .env("PSExecutionPolicyPreference", "Restricted")
        .creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let received: Vec<String> = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert_eq!(
        received,
        [
            "mcp",
            "add",
            "spacememory",
            "--",
            mem_exe.to_str().unwrap(),
            "mcp"
        ]
    );
    assert_eq!(std::fs::read_to_string(policy_receipt).unwrap(), "Bypass");
    assert!(has_windows_config_entry(&config, &mem_exe));
}

#[cfg(windows)]
fn ps_literal(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(windows)]
fn has_windows_config_entry(config: &std::path::Path, mem: &std::path::Path) -> bool {
    let text = std::fs::read_to_string(config).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    parsed["mcp_servers"]["spacememory"]["command"].as_str() == mem.to_str()
        && parsed["mcp_servers"]["spacememory"]["args"][0].as_str() == Some("mcp")
}

#[test]
fn bare_mcp_still_serves_initialize_and_tools_list() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("mem"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"test","version":"1"}}}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let responses: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 2, "{text}");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[1]["id"], 2);
    assert!(responses[1]["result"]["tools"].as_array().unwrap().len() > 0);
}
