//! `mem mcp install`：Agent 宿主 MCP 注册与通用引导。

use mem_core::platform::process::ProcessCommand;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const SERVICE: &str = "spacememory";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    Codex,
    ClaudeCode,
}

impl Client {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude-code" => Ok(Self::ClaudeCode),
            _ => Err(format!(
                "不支持的客户端“{value}”；目前支持 codex、claude-code"
            )),
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }

    fn config_path(self) -> Result<PathBuf, String> {
        match self {
            Self::Codex => {
                let root = std::env::var_os("CODEX_HOME")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| home_dir().map(|h| h.join(".codex")))
                    .ok_or_else(|| {
                        "无法确定 Codex 用户配置目录（CODEX_HOME 或用户主目录）".to_string()
                    })?;
                Ok(root.join("config.toml"))
            }
            Self::ClaudeCode => {
                let root = match std::env::var_os("CLAUDE_CONFIG_DIR") {
                    Some(p) if !p.is_empty() => PathBuf::from(p),
                    Some(_) => {
                        return Err(
                            "CLAUDE_CONFIG_DIR 为空，无法确定 Claude Code 用户配置位置".into()
                        );
                    }
                    None => home_dir().ok_or_else(|| {
                        "无法确定 Claude Code 用户配置目录（CLAUDE_CONFIG_DIR 或用户主目录）"
                            .to_string()
                    })?,
                };
                Ok(root.join(".claude.json"))
            }
        }
    }

    fn add_args(self, mem_exe: &OsStr) -> Vec<OsString> {
        let mut a: Vec<OsString> = match self {
            Self::Codex => ["mcp", "add", SERVICE, "--"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            Self::ClaudeCode => [
                "mcp",
                "add",
                "--transport",
                "stdio",
                "--scope",
                "user",
                SERVICE,
                "--",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        };
        a.push(mem_exe.to_os_string());
        a.push(OsString::from("mcp"));
        a
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub fn print_help() {
    println!(
        "SpaceMemory MCP 安装引导\n\nMCP 让 Agent 调用本机 SpaceMemory 工具。安装只登记启动入口，不创建记忆店，也不改动记录。运行时使用 $MEM_STORE 或 ~/.agent-memory。\n\n通用 stdio 启动参数：服务名 spacememory；命令为本机 mem 可执行文件；参数数组 [\"mcp\"]。不同 Agent 的配置文件和安装方式不同，请按对应宿主处理。\n\n命令：\n  mem mcp install                           输出通用 Agent 引导\n  mem mcp install --client codex            安装到 Codex 用户配置\n  mem mcp install --client claude-code      安装到 Claude Code 用户配置\n  mem mcp install --client <名称> --dry-run 只显示计划，不写配置\n\n安装会授予 Agent 使用 SpaceMemory 读写工具的能力。项目级配置可能覆盖用户级服务。客户端可能需要重启或新开会话后加载服务。\n"
    );
}

pub fn run(install: bool, client: Option<&str>, dry_run: bool) -> Result<(), String> {
    if !install {
        print_help();
        return Ok(());
    }
    let Some(client) = client.filter(|s| !s.is_empty()) else {
        return print_generic_guide(
            &std::env::current_exe().map_err(|e| format!("无法确定 mem 可执行文件路径：{e}"))?,
        );
    };
    let client = Client::parse(client)?;
    let mem_exe =
        std::env::current_exe().map_err(|e| format!("无法确定 mem 可执行文件路径：{e}"))?;
    if mem_exe.to_str().is_none() {
        return Err(
            "mem 可执行文件路径无法表示为客户端配置支持的 Unicode 文本；请移动或重装后重试".into(),
        );
    }
    let config = client.config_path()?;
    let cli = find_executable(client.executable()).ok_or_else(|| {
        manual_instructions(
            client,
            &mem_exe,
            &config,
            "找不到客户端命令；未运行客户端 CLI",
        )
    })?;
    install_with_prefix(
        client,
        dry_run,
        &config,
        &cli.program,
        &cli.prefix_args,
        &mem_exe,
        &mut |program, args| {
            let status = std::process::Command::new(program).args(args).status()?;
            Ok(status.success())
        },
    )
}

pub fn print_generic_guide(mem_exe: &Path) -> Result<(), String> {
    let path = mem_exe
        .to_str()
        .ok_or("mem 可执行文件路径无法无损表示为 Unicode 文本；无法生成通用引导")?;
    let path = serde_json::to_string(path).map_err(|e| format!("无法编码 mem 路径：{e}"))?;
    println!(
        "请帮我把本机的 SpaceMemory 接入当前 Agent，按以下步骤完成并验证：\n\n1. 先识别当前 Agent 宿主及其本机 MCP 配置方式；不要假定具体 Agent，也不要只凭项目目录猜。\n2. SpaceMemory MCP 启动信息：服务名称 `spacememory`；传输方式 `stdio`；启动命令（JSON 字符串）是 `{path}`；参数数组 `[\"mcp\"]`；不要求额外环境变量或特定工作目录。这里是一组通用启动参数，不是通用配置文件格式。\n3. 确认宿主支持本机 stdio MCP 后，按宿主自己的方式登记。先检查目标范围是否已有同名配置；发现已有同名服务时停止，说明现状和处理选项，不自动覆盖。检查后另一程序仍可能同时添加同名服务，客户端行为可能替换该并发添加。\n4. 安装后按宿主提供的方式确认服务已连接、SpaceMemory 工具可见；若当前会话不能热加载，说明如何重启或开启新会话。\n5. 无法识别宿主、不支持本机 stdio 或安装失败时，先说明原因和所需信息；不要猜测客户端命令或修改配置。\n\n安装只添加 Agent 的本机服务启动配置。它不会创建记忆店；服务运行时以当前用户身份启动，并可能调用 SpaceMemory 读写工具。\n"
    );
    Ok(())
}

fn find_executable(name: &str) -> Option<ProcessCommand> {
    mem_core::platform::process::resolve_client_command(name, std::env::var_os("PATH").as_deref())
}

fn has_service(client: Client, path: &Path) -> Result<bool, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("无法读取客户端用户配置 {}：{e}", path.display())),
    };
    match client {
        Client::Codex => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| format!("Codex 配置不是有效 UTF-8：{e}"))?;
            let root: toml::Value =
                toml::from_str(text).map_err(|e| format!("Codex TOML 配置无法解析：{e}"))?;
            let Some(servers) = root.get("mcp_servers") else {
                return Ok(false);
            };
            let table = servers
                .as_table()
                .ok_or("Codex 配置 mcp_servers 不是表格，已停止以免覆盖未知结构")?;
            Ok(table.contains_key(SERVICE))
        }
        Client::ClaudeCode => {
            let root: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Claude Code JSON 配置无法解析：{e}"))?;
            if !root.is_object() {
                return Err("Claude Code 配置根节点不是 JSON 对象，已停止以免覆盖未知结构".into());
            }
            let Some(servers) = root.get("mcpServers") else {
                return Ok(false);
            };
            let object = servers
                .as_object()
                .ok_or("Claude Code 配置 mcpServers 不是对象，已停止以免覆盖未知结构")?;
            Ok(object.contains_key(SERVICE))
        }
    }
}

fn has_expected_service(
    client: Client,
    path: &Path,
    expected_command: &str,
) -> Result<bool, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("无法读取客户端用户配置 {}：{e}", path.display())),
    };
    match client {
        Client::Codex => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| format!("Codex 配置不是有效 UTF-8：{e}"))?;
            let root: toml::Value =
                toml::from_str(text).map_err(|e| format!("Codex TOML 配置无法解析：{e}"))?;
            let Some(servers) = root.get("mcp_servers") else {
                return Ok(false);
            };
            let table = servers
                .as_table()
                .ok_or("Codex 配置 mcp_servers 不是表格，已停止以免覆盖未知结构")?;
            let Some(entry) = table.get(SERVICE) else {
                return Ok(false);
            };
            Ok(
                entry.get("command").and_then(toml::Value::as_str) == Some(expected_command)
                    && entry
                        .get("args")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|args| args.len() == 1 && args[0].as_str() == Some("mcp")),
            )
        }
        Client::ClaudeCode => {
            let root: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Claude Code JSON 配置无法解析：{e}"))?;
            if !root.is_object() {
                return Err("Claude Code 配置根节点不是 JSON 对象，已停止以免覆盖未知结构".into());
            }
            let Some(servers) = root.get("mcpServers") else {
                return Ok(false);
            };
            let object = servers
                .as_object()
                .ok_or("Claude Code 配置 mcpServers 不是对象，已停止以免覆盖未知结构")?;
            let Some(entry) = object.get(SERVICE) else {
                return Ok(false);
            };
            Ok(
                entry.get("type").and_then(serde_json::Value::as_str) == Some("stdio")
                    && entry.get("command").and_then(serde_json::Value::as_str)
                        == Some(expected_command)
                    && entry
                        .get("args")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|args| args.len() == 1 && args[0].as_str() == Some("mcp")),
            )
        }
    }
}

#[cfg(test)]
fn install_with_paths<F>(
    client: Client,
    dry_run: bool,
    config: &Path,
    cli: &Path,
    mem_exe: &Path,
    run_cli: &mut F,
) -> Result<(), String>
where
    F: FnMut(&OsStr, &[OsString]) -> std::io::Result<bool>,
{
    install_with_prefix(client, dry_run, config, cli, &[], mem_exe, run_cli)
}

fn install_with_prefix<F>(
    client: Client,
    dry_run: bool,
    config: &Path,
    cli: &Path,
    prefix_args: &[OsString],
    mem_exe: &Path,
    run_cli: &mut F,
) -> Result<(), String>
where
    F: FnMut(&OsStr, &[OsString]) -> std::io::Result<bool>,
{
    let command = mem_exe
        .to_str()
        .ok_or("mem 可执行文件路径无法表示为 Unicode 文本")?;
    let mut args = prefix_args.to_vec();
    args.extend(client.add_args(mem_exe.as_os_str()));
    if dry_run {
        println!(
            "预演：将调用 {} {:?}\n配置范围：用户级\n配置文件：{}\n不会写配置或启动 MCP 服务。",
            cli.display(),
            args,
            config.display()
        );
        return Ok(());
    }
    let existing = has_service(client, config).map_err(|error| {
        manual_instructions(
            client,
            mem_exe,
            config,
            &format!("{error}；未运行客户端 CLI"),
        )
    })?;
    if existing {
        return Err(format!(
            "用户配置中已存在名为 {SERVICE} 的服务；配置没有修改。请先检查现有设置，再决定是否手动移除或使用其他名称。"
        ));
    }
    if !run_cli(cli.as_os_str(), &args).map_err(|e| {
        manual_instructions(
            client,
            mem_exe,
            config,
            &format!("无法启动 {}：{e}", cli.display()),
        )
    })? {
        return Err(manual_instructions(
            client,
            mem_exe,
            config,
            &format!("{} 添加服务失败（退出码非零）", cli.display()),
        ));
    }
    let expected = has_expected_service(client, config, command).map_err(|error| {
        manual_instructions(
            client,
            mem_exe,
            config,
            &format!("写入后无法读取配置：{error}"),
        )
    })?;
    if !expected {
        return Err(manual_instructions(
            client,
            mem_exe,
            config,
            &format!("{} 返回成功，但配置没有读回预期命令和参数", cli.display()),
        ));
    }
    println!("{}", success_message(client));
    Ok(())
}

fn success_message(client: Client) -> String {
    let (client_name, concurrent_note) = match client {
        Client::Codex => (
            "Codex",
            "预检不能阻止另一程序在检查后同时添加同名服务；Codex 命令可能替换并发添加的配置。",
        ),
        Client::ClaudeCode => (
            "Claude Code",
            "若另一程序在预检后同时登记同名服务，Claude Code 可能拒绝本次添加。",
        ),
    };
    format!(
        "已登记 {SERVICE} 到 {client_name} 用户配置。若当前 Agent 未显示工具，请重启客户端或开启新会话。项目级同名配置可能覆盖此用户级设置。{concurrent_note} SpaceMemory MCP 可提供会改动记忆数据的工具。"
    )
}

fn manual_instructions(client: Client, mem_exe: &Path, config: &Path, reason: &str) -> String {
    let executable = mem_exe.to_str().unwrap_or("<不可表示路径>");
    let args = client.add_args(mem_exe.as_os_str());
    let args_json = serde_json::to_string_pretty(
        &args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".into());
    let verification = match client {
        Client::Codex => "codex mcp list（此命令仅供你主动检查）",
        Client::ClaudeCode => "claude mcp list（会启动服务做连接检查，仅在你主动选择时运行）",
    };
    let snippet = match client {
        Client::Codex => {
            let mut server = toml::map::Map::new();
            server.insert(
                "command".into(),
                toml::Value::String(executable.to_string()),
            );
            server.insert(
                "args".into(),
                toml::Value::Array(vec![toml::Value::String("mcp".into())]),
            );
            let mut servers = toml::map::Map::new();
            servers.insert(SERVICE.into(), toml::Value::Table(server));
            let mut root = toml::map::Map::new();
            root.insert("mcp_servers".into(), toml::Value::Table(servers));
            toml::to_string(&toml::Value::Table(root)).unwrap_or_default()
        }
        Client::ClaudeCode => {
            serde_json::to_string_pretty(&serde_json::json!({"mcpServers": {SERVICE: {
                "type": "stdio", "command": executable, "args": ["mcp"]
            }}}))
            .unwrap_or_default()
        }
    };
    format!(
        "{reason}。安装结果未确认；请先核对配置文件，避免重复添加。\n\n可供 Agent 或你手动处理的调用信息：程序 `{}`，参数数组：\n{args_json}\n\n目标用户配置文件：{}\n手动配置片段：\n{snippet}\n\n登记后可用此方式检查：{verification}\n若当前 Agent 未加载服务，请重启客户端或开启新会话。",
        client.executable(),
        config.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parser_distinguishes_missing_existing_and_invalid_files() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex.toml");
        assert!(!has_service(Client::Codex, &codex).unwrap());
        std::fs::write(&codex, "[mcp_servers.spacememory]\ncommand='old'\n").unwrap();
        assert!(has_service(Client::Codex, &codex).unwrap());
        std::fs::write(&codex, "mcp_servers = 3\n").unwrap();
        assert!(has_service(Client::Codex, &codex).is_err());

        let claude = temp.path().join("claude.json");
        std::fs::write(
            &claude,
            br#"{"mcpServers":{"spacememory":{"command":"old"}}}"#,
        )
        .unwrap();
        assert!(has_service(Client::ClaudeCode, &claude).unwrap());
        std::fs::write(&claude, b"{").unwrap();
        assert!(has_service(Client::ClaudeCode, &claude).is_err());
        std::fs::write(&claude, b"[]").unwrap();
        assert!(has_service(Client::ClaudeCode, &claude).is_err());
    }

    #[test]
    fn client_arguments_are_separate_values_and_paths_round_trip() {
        let path = PathBuf::from("/tmp/a path/你好\\mem");
        let args = Client::Codex.add_args(path.as_os_str());
        assert_eq!(args[0], "mcp");
        assert_eq!(args[1], "add");
        assert_eq!(args[3], "--");
        assert_eq!(args[4], path.as_os_str());
        let encoded = serde_json::to_string(path.to_str().unwrap()).unwrap();
        assert_eq!(
            serde_json::from_str::<String>(&encoded).unwrap(),
            path.to_str().unwrap()
        );
        let claude = Client::ClaudeCode.add_args(path.as_os_str());
        let claude: Vec<String> = claude
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            claude,
            vec![
                "mcp",
                "add",
                "--transport",
                "stdio",
                "--scope",
                "user",
                "spacememory",
                "--",
                "/tmp/a path/你好\\mem",
                "mcp"
            ]
        );
    }

    #[test]
    fn existing_service_stops_before_client_cli_is_called() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, "[mcp_servers.spacememory]\ncommand='old'\n").unwrap();
        let mut calls = 0;
        let result = install_with_paths(
            Client::Codex,
            false,
            &config,
            Path::new("fake-codex"),
            Path::new("/tmp/mem"),
            &mut |_, _| {
                calls += 1;
                Ok(true)
            },
        );
        assert!(result.unwrap_err().contains("已存在"));
        assert_eq!(calls, 0);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[mcp_servers.spacememory]\ncommand='old'\n"
        );
    }

    #[test]
    fn unreadable_or_malformed_preflight_stops_without_calling_client() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, "not valid toml [[").unwrap();
        let mut calls = 0;
        let result = install_with_paths(
            Client::Codex,
            false,
            &config,
            Path::new("fake-codex"),
            Path::new("/tmp/mem"),
            &mut |_, _| {
                calls += 1;
                Ok(true)
            },
        );
        assert!(result.unwrap_err().contains("无法解析"));
        assert_eq!(calls, 0);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "not valid toml [["
        );
    }

    #[test]
    fn installer_reads_back_config_after_cli_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let cli = Path::new("fake-claude");
        let mem = Path::new("/tmp/mem");
        let prefix = vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            OsString::from("/tmp/Claude Code/claude.ps1"),
        ];
        let mut received = Vec::new();
        let result = install_with_prefix(
            Client::ClaudeCode,
            false,
            &config,
            cli,
            &prefix,
            mem,
            &mut |_, args| {
                received = args.to_vec();
                assert_eq!(&args[..prefix.len()], prefix.as_slice());
                assert!(args.iter().any(|a| a == "--scope"));
                std::fs::write(
                    &config,
                    br#"{"mcpServers":{"spacememory":{"type":"stdio","command":"/tmp/mem","args":["mcp"]}}}"#,
                )?;
                Ok(true)
            },
        );
        assert!(result.is_ok());
        assert_eq!(&received[..prefix.len()], prefix.as_slice());
        assert_eq!(received[prefix.len()], "mcp");
        assert_eq!(received[prefix.len() + 7], "--");
        assert_eq!(received[prefix.len() + 8], "/tmp/mem");
        assert_eq!(received[prefix.len() + 9], "mcp");
    }

    #[test]
    fn successful_cli_with_missing_or_wrong_readback_is_not_reported_as_installed() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let result = install_with_paths(
            Client::ClaudeCode,
            false,
            &config,
            Path::new("fake-claude"),
            Path::new("/tmp/mem"),
            &mut |_, _| Ok(true),
        );
        assert!(result.unwrap_err().contains("没有读回"));

        std::fs::write(
            &config,
            br#"{"mcpServers":{"spacememory":{"type":"stdio","command":"/other/mem","args":["mcp"]}}}"#,
        )
        .unwrap();
        assert!(!has_expected_service(Client::ClaudeCode, &config, "/tmp/mem").unwrap());
    }

    #[test]
    fn dry_run_does_not_parse_or_modify_config_or_run_client() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, b"not valid toml [[").unwrap();
        let before = std::fs::read(&config).unwrap();
        let mut calls = 0;
        let result = install_with_paths(
            Client::Codex,
            true,
            &config,
            Path::new("fake-codex"),
            Path::new("/tmp/mem"),
            &mut |_, _| {
                calls += 1;
                Ok(true)
            },
        );
        assert!(result.is_ok());
        assert_eq!(calls, 0);
        assert_eq!(std::fs::read(&config).unwrap(), before);
    }

    #[test]
    fn codex_write_is_verified_against_exact_command_and_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let result = install_with_paths(
            Client::Codex,
            false,
            &config,
            Path::new("fake-codex"),
            Path::new("/tmp/mem"),
            &mut |_, _| {
                std::fs::write(
                    &config,
                    "[mcp_servers.spacememory]\ncommand='/tmp/mem'\nargs=['mcp']\n",
                )?;
                Ok(true)
            },
        );
        assert!(result.is_ok());
        assert!(has_expected_service(Client::Codex, &config, "/tmp/mem").unwrap());
        assert!(!has_expected_service(Client::Codex, &config, "/other/mem").unwrap());
    }

    #[test]
    fn cli_name_parser_rejects_unknown_values() {
        assert!(Client::parse("codex").is_ok());
        assert!(Client::parse("claude-code").is_ok());
        assert!(
            Client::parse("other")
                .unwrap_err()
                .contains("codex、claude-code")
        );
    }

    #[test]
    fn success_messages_name_the_client_before_describing_the_race() {
        let codex = success_message(Client::Codex);
        assert!(codex.contains("到 Codex 用户配置。"), "{codex}");
        assert!(codex.find("到 Codex 用户配置").unwrap() < codex.find("预检不能阻止").unwrap());
        let claude = success_message(Client::ClaudeCode);
        assert!(claude.contains("到 Claude Code 用户配置。"), "{claude}");
        assert!(claude.find("到 Claude Code 用户配置").unwrap() < claude.find("预检后").unwrap());
    }

    #[test]
    fn codex_preflight_race_is_explicitly_accepted_and_warned() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let result = install_with_paths(
            Client::Codex,
            false,
            &config,
            Path::new("fake-codex"),
            Path::new("/tmp/mem"),
            &mut |_, _| {
                // 交错：预检已通过后，另一个写入者添加同名配置；模拟 Codex add 将其替换。
                std::fs::write(
                    &config,
                    "[mcp_servers.spacememory]\ncommand='/concurrent/mem'\nargs=['mcp']\n",
                )?;
                std::fs::write(
                    &config,
                    "[mcp_servers.spacememory]\ncommand='/tmp/mem'\nargs=['mcp']\n",
                )?;
                Ok(true)
            },
        );
        assert!(result.is_ok());
        assert!(has_expected_service(Client::Codex, &config, "/tmp/mem").unwrap());
        assert!(success_message(Client::Codex).contains("可能替换并发添加的配置"));
    }
}
