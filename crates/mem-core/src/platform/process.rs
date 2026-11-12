//! PATH 中可直接启动的程序解析；脚本入口使用显式解释器和分离参数，不拼 shell 命令。

use std::ffi::{OsStr, OsString};
#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessCommand {
    pub program: PathBuf,
    pub prefix_args: Vec<OsString>,
}

/// 按 PATH 顺序选择可以由当前平台直接启动的客户端入口。
pub fn resolve_client_command(name: &str, path: Option<&OsStr>) -> Option<ProcessCommand> {
    let dirs: Vec<PathBuf> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    #[cfg(windows)]
    {
        let powershell = powershell_from_path(&dirs).or_else(powershell_from_system_root);
        resolve_windows_entry(name, &dirs, powershell.as_deref())
    }
    #[cfg(unix)]
    {
        resolve_unix_entry(name, &dirs)
    }
    #[cfg(not(any(unix, windows)))]
    {
        dirs.into_iter()
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .map(|program| ProcessCommand {
                program,
                prefix_args: Vec::new(),
            })
    }
}

#[cfg(unix)]
fn resolve_unix_entry(name: &str, dirs: &[PathBuf]) -> Option<ProcessCommand> {
    use std::os::unix::fs::PermissionsExt;

    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
                .unwrap_or(false)
        })
        .map(|program| ProcessCommand {
            program,
            prefix_args: Vec::new(),
        })
}

/// 纯 Windows 入口选择器单独可测，在非 Windows 主机上也验证 `.exe` / npm `.ps1` 规则。
#[cfg(any(windows, test))]
fn resolve_windows_entry(
    name: &str,
    dirs: &[PathBuf],
    powershell: Option<&Path>,
) -> Option<ProcessCommand> {
    for dir in dirs {
        let native = dir.join(format!("{name}.exe"));
        if native.is_file() {
            return Some(ProcessCommand {
                program: native,
                prefix_args: Vec::new(),
            });
        }

        // npm 会为同一个 JS CLI 生成 .ps1 与 .cmd。对这个子进程指定 Bypass：只影响
        // 当前进程，不写入注册表；组策略仍然优先。-File 后的路径与参数仍由 Command
        // 逐项传入；不通过 cmd /c 拼接，也不执行 .cmd 文件。
        let script = dir.join(format!("{name}.ps1"));
        if script.is_file() {
            if let Some(powershell) = powershell {
                return Some(ProcessCommand {
                    program: powershell.to_path_buf(),
                    prefix_args: vec![
                        OsString::from("-NoProfile"),
                        OsString::from("-NonInteractive"),
                        OsString::from("-ExecutionPolicy"),
                        OsString::from("Bypass"),
                        OsString::from("-File"),
                        script.into_os_string(),
                    ],
                });
            }
        }
    }
    None
}

#[cfg(windows)]
fn powershell_from_path(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join("powershell.exe"))
        .find(|p| p.is_file())
}

#[cfg(windows)]
fn powershell_from_system_root() -> Option<PathBuf> {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .filter(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn windows_resolver_uses_powershell_for_npm_script_shim() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin with spaces");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("codex.ps1");
        std::fs::write(&script, "# npm PowerShell shim").unwrap();
        let powershell =
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");

        let plan = resolve_windows_entry("codex", &[bin], Some(&powershell)).unwrap();
        assert_eq!(plan.program, powershell);
        assert_eq!(
            plan.prefix_args,
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-ExecutionPolicy"),
                OsString::from("Bypass"),
                OsString::from("-File"),
                script.into_os_string(),
            ]
        );
    }

    #[test]
    fn windows_resolver_prefers_native_exe_and_skips_cmd_shims() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("codex.cmd"), "@echo off").unwrap();
        std::fs::write(second.join("codex.exe"), "native exe placeholder").unwrap();
        let powershell =
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");

        let plan =
            resolve_windows_entry("codex", &[first, second.clone()], Some(&powershell)).unwrap();
        assert_eq!(plan.program, second.join("codex.exe"));
        assert!(plan.prefix_args.is_empty());

        std::fs::write(second.join("codex.ps1"), "# npm PowerShell shim").unwrap();
        std::fs::remove_file(second.join("codex.exe")).unwrap();
        let plan = resolve_windows_entry("codex", &[second.clone()], Some(&powershell)).unwrap();
        assert_eq!(plan.program, powershell);
        assert_eq!(
            plan.prefix_args[5],
            second.join("codex.ps1").into_os_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_resolver_skips_non_executable_file_and_uses_later_command() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let blocked = first.join("codex");
        let executable = second.join("codex");
        std::fs::write(&blocked, "not executable").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();

        let plan = resolve_unix_entry("codex", &[first, second]).unwrap();
        assert_eq!(plan.program, executable);
        assert!(plan.prefix_args.is_empty());
    }
}
