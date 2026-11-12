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
