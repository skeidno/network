//! winget 缺失时的诊断与修复（只在用户明确点「修复 winget」时执行）。
//!
//! 全新 Windows / LTSC / Server 上 App Installer 还没下发，winget 的「应用执行别名」
//! 文件根本不存在。微软官方文档给出的离线修复方式是：从 winget-cli 官方 release
//! 下载 msixbundle + License1.xml + DesktopAppInstaller_Dependencies.zip，
//! 按 VCLibs → UI.Xaml / WindowsAppRuntime → 主体的顺序 `Add-AppxPackage` 注册。
//! 缺依赖时 Add-AppxPackage 会静默失败（HRESULT 0x80073CF3），winget.exe 不会出现。
//!
//! 三条分支，按代价从小到大：
//! 1. 别名在、命令跑不通（Store 未初始化）或包已注册但别名丢了 → 零下载重注册；
//! 2. 包确实没有 → 下载官方三件套，逐个校验 SHA256 后注册；
//! 3. 下载/注册失败 → 原样返回日志，上层自动降级到官方直连安装。
//!
//! 下载源：GitHub release 是唯一官方发布点，但全新机器往往还没导入节点配置、
//! 也没开系统代理，直连 GitHub 必然超时。因此按「官方直连 → 官方走本程序
//! mixed-port → 国内镜像」依次尝试；镜像只是同一文件的转发副本，SHA256 钉死在
//! 官方 digest 上，内容不符会在校验阶段被拦下，不会注册进系统。
//!
//! 版本和哈希全部钉死在 winget-cli v1.29.380 官方 release 上（取自 GitHub API 的
//! `digest` 字段），不追 latest：包管理器自身升级由 Windows 商店负责，我们只负责
//! 把「从零到能用」这一步修好。下载文件放在临时目录，校验通过才交给系统安装。

#[cfg(windows)]
mod imp {
    use crate::envdirect::{ps_quote, run_powershell, temp_work_dir};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::Read;
    use std::path::Path;

    const TAG: &str = "v1.29.380";
    const BASE: &str = "https://github.com/microsoft/winget-cli/releases/download/v1.29.380";
    const BUNDLE: &str = "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe.msixbundle";
    const DEPS: &str = "DesktopAppInstaller_Dependencies.zip";
    const LICENSE: &str = "e53e159d00e04f729cc2180cffd1c02e_License1.xml";
    /// 国内可达的 GitHub release 转发镜像。实测 2026-11-09：三个源都返回
    /// 与官方 `License1.xml` 完全一致的字节（SHA256 相同）。不写进配置、不追
    /// latest，避免第三方域名哪天失效后还必须改代码才能用。
    const MIRRORS: &[&str] = &[
        "https://ghfast.top/",
        "https://gh-proxy.com/",
        "https://ghproxy.net/",
    ];

    pub(super) const DOWNLOAD_GUIDE: &str = "GitHub release 与国内镜像均无法下载或校验失败。\
常见原因是这台机器还没有导入节点配置 / 没有开启接管，或系统代理未配置。\n\
修复 winget 是可选增强：环境页的安装、更新、版本查询已自动走官方直连通道，不受影响。\n\
仍要修复的话：先在程序里导入配置并开启接管（下载会自动使用本机代理端口），或配好网络后重试。";

    // GitHub release API 的 digest 字段（sha256），2026-11-09 核对。
    pub(super) const PAYLOADS: &[(&str, &str)] = &[
        (
            "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe.msixbundle",
            "65dea9c01ce08ee7b763366b27c0e651f97db857c11ca9b9c301826c10092f2e",
        ),
        (
            "DesktopAppInstaller_Dependencies.zip",
            "ba875afe9d190f61218985ac0292a99d1db710bf93e13c68944ca9d89f0d82d1",
        ),
        (
            "e53e159d00e04f729cc2180cffd1c02e_License1.xml",
            "bcb15118ec47df24e3e6013a7006147c3a15b3a8104ed660fe87c8b4ed01f485",
        ),
    ];

    pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
        let mut file = fs::File::open(path).map_err(|error| format!("读取 {path:?} 失败：{error}"))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("读取下载文件失败：{error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
    }

    fn official_url(name: &str) -> String {
        format!("{BASE}/{name}")
    }

    /// 下载尝试顺序。官方优先；本机内核在跑时加一次「官方 + 本程序 mixed-port」；
    /// 最后才是镜像。镜像与代理不做笛卡尔积，避免 217MB 的包被反复下载。
    pub(super) fn download_attempts(
        name: &str,
        proxy: Option<&str>,
    ) -> Vec<(String, Option<String>)> {
        let official = official_url(name);
        let mut attempts = vec![(official.clone(), None)];
        if let Some(proxy) = proxy {
            attempts.push((official.clone(), Some(proxy.to_string())));
        }
        for mirror in MIRRORS {
            attempts.push((format!("{mirror}{official}"), None));
        }
        attempts
    }

    /// 本程序自己就是代理软件：内核起来后 mixed-port 一定在听，把它作为下载
    /// 代理比让用户先去配系统代理靠谱。端口没通就返回 None，不影响其它源。
    fn local_proxy() -> Option<String> {
        let port = crate::store::load().mixed_port;
        if port == 0 {
            return None;
        }
        let address = format!("127.0.0.1:{port}");
        let socket = address.parse::<std::net::SocketAddr>().ok()?;
        std::net::TcpStream::connect_timeout(&socket, std::time::Duration::from_millis(300))
            .is_ok()
            .then(|| format!("http://{address}"))
    }

    /// 本地已有且哈希吻合就直接复用：315MB 不必因为中途失败重下一遍。
    fn ensure_payload(
        dir: &Path,
        name: &str,
        expected: &str,
        proxy: Option<&str>,
    ) -> Result<String, String> {
        let dest = dir.join(name);
        if dest.is_file() {
            match sha256_file(&dest) {
                Ok(actual) if actual == expected => {
                    return Ok(format!("{name}：已存在且 SHA256 校验通过，跳过下载"));
                }
                Ok(actual) => {
                    let _ = fs::remove_file(&dest);
                    return Err(format!("{name} 本地文件校验不通过，已删除待重下：{actual}"));
                }
                Err(error) => return Err(error),
            }
        }
        let mut failures = Vec::new();
        for (url, via) in download_attempts(name, proxy) {
            let label = match &via {
                Some(proxy) => format!("{url}（经本机代理 {proxy}）"),
                None => url.clone(),
            };
            match crate::envdirect::download_file_with(&url, &dest, via.as_deref()) {
                Ok(size) => match sha256_file(&dest) {
                    Ok(actual) if actual == expected => {
                        return Ok(format!(
                            "{name}：{label} 下载 {:.1} MB，SHA256 校验通过",
                            size as f64 / 1024.0 / 1024.0
                        ));
                    }
                    Ok(actual) => {
                        // 镜像返回了 HTML 错误页之类的东西：丢弃再试下一个源。
                        failures.push(format!("{label}：SHA256 不符（{actual}），已丢弃"));
                        let _ = fs::remove_file(&dest);
                    }
                    Err(error) => {
                        failures.push(format!("{label}：{error}"));
                        let _ = fs::remove_file(&dest);
                    }
                },
                Err(error) => failures.push(format!(
                    "{label}：{}",
                    error.lines().next().unwrap_or(error.as_str())
                )),
            }
        }
        Err(format!(
            "{name} 所有下载源均失败：\n{}\n\n{DOWNLOAD_GUIDE}",
            failures.join("\n")
        ))
    }

    /// 诊断：别名在不在、App Installer 包注册了没、命令能不能跑。
    /// 三个状态对应三条修复路径，不要只用一个 bool 猜。
    fn diagnose() -> Value {
        let alias = crate::envmgr::winget_path();
        let package = run_powershell(
            "(Get-AppxPackage -Name Microsoft.DesktopAppInstaller | Select-Object -First 1).InstallLocation",
        )
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
        let version = alias.as_ref().and_then(|exe| {
            let output = crate::envdirect::hidden_command(exe)
                .args(["--version"])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .filter(|text| !text.is_empty())
        });
        let state = if version.is_some() {
            "ok"
        } else if alias.is_some() {
            "broken"
        } else if package.is_some() {
            "package-only"
        } else {
            "missing"
        };
        json!({
            "state": state,
            "version": version,
            "alias": alias.as_ref().map(|p| p.to_string_lossy().to_string()),
            "package": package,
        })
    }

    fn winget_version() -> Option<String> {
        let exe = crate::envmgr::winget_path()?;
        let output = crate::envdirect::hidden_command(&exe)
            .args(["--version"])
            .output()
            .ok()?;
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        })
    }

    /// 注册完不立刻下结论：别名是 Store 异步落地的，重试几次再判失败。
    fn verify() -> Option<String> {
        for _ in 0..3 {
            if let Some(version) = winget_version() {
                return Some(version);
            }
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        None
    }

    /// 包已注册但别名丢了 / CLI 跑不通：重注册比重新下载 315MB 合理得多。
    fn reregister(location: &str) -> Result<String, String> {
        let manifest = format!("{}\\AppxManifest.xml", location.trim_end_matches('\\'));
        let script = format!(
            "Add-AppxPackage -Register '{}' -DisableDevelopmentMode -ErrorAction Stop; 'REREGISTER_OK'",
            ps_quote(&manifest)
        );
        let text = run_powershell(&script)
            .map_err(|error| format!("重注册 App Installer 失败：{error}"))?;
        match verify() {
            Some(version) => Ok(format!("App Installer 已重新注册，winget {version} 可用，无需下载。\n{text}")),
            None => Err(format!("重注册已执行但 winget 仍不可用：\n{text}")),
        }
    }

    pub(super) fn register_script(dir: &Path) -> String {
        let bundle = ps_quote(&dir.join(BUNDLE).to_string_lossy());
        let zip = ps_quote(&dir.join(DEPS).to_string_lossy());
        let license = ps_quote(&dir.join(LICENSE).to_string_lossy());
        let deps = ps_quote(&dir.join("deps").to_string_lossy());
        format!(
            r#"$ErrorActionPreference='Continue'
$bundle='{bundle}'
$zip='{zip}'
$license='{license}'
$deps='{deps}'
if (Test-Path $deps) {{ Remove-Item -LiteralPath $deps -Recurse -Force }}
Expand-Archive -LiteralPath $zip -DestinationPath $deps -Force
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {{ 'arm64' }} elseif ($env:PROCESSOR_ARCHITECTURE -eq 'x86') {{ 'x86' }} else {{ 'x64' }}
$pkgs = @(Get-ChildItem -LiteralPath $deps -Recurse -File | Where-Object {{ $_.Extension -in @('.appx','.msix') -and $_.FullName -like "*$arch*" }})
if ($pkgs.Count -eq 0) {{ $pkgs = @(Get-ChildItem -LiteralPath $deps -Recurse -File | Where-Object {{ $_.Extension -in @('.appx','.msix') }}) }}
$pkgs = @($pkgs | Sort-Object @{{Expression={{ if ($_.Name -like '*VCLibs*') {{ 0 }} elseif ($_.Name -like '*Xaml*') {{ 1 }} else {{ 2 }} }} }}, Name)
"DEPENDENCIES $($pkgs.Count)"
foreach ($p in $pkgs) {{
  try {{ Add-AppxPackage -Path $p.FullName -ErrorAction Stop | Out-Null; "DEP_OK $($p.Name)" }}
  catch {{ "DEP_SKIP $($p.Name) :: $($_.Exception.Message)" }}
}}
try {{ Add-AppxPackage -Path $bundle -LicensePath $license -ErrorAction Stop }}
catch {{ "LICENSE_PATH_FAILED :: $($_.Exception.Message)"; Add-AppxPackage -Path $bundle -ErrorAction Stop }}
'BUNDLE_OK'"#
        )
    }

    /// 主入口。返回给界面的是完整过程日志；失败时也带日志，方便直接判断
    /// 是 GitHub 不可达、哈希不符，还是系统依赖注册失败。
    pub fn repair() -> Result<String, String> {
        let before = diagnose();
        let state = before["state"].as_str().unwrap_or("missing");
        let mut log = vec![format!(
            "诊断：state={state}，winget 别名={}，App Installer 包={}",
            before["alias"].as_str().unwrap_or("无"),
            before["package"].as_str().unwrap_or("未注册")
        )];
        if state == "ok" {
            log.push(format!(
                "winget {} 已经可用，无需修复。",
                before["version"].as_str().unwrap_or("")
            ));
            return Ok(log.join("\n"));
        }

        // 包在就先零下载重注册；失败才继续走完整安装，不把 315MB 当第一选择。
        if let Some(location) = before["package"].as_str() {
            log.push("检测到 App Installer 包已注册，先尝试零下载重注册。".into());
            match reregister(location) {
                Ok(detail) => {
                    log.push(detail);
                    return Ok(log.join("\n"));
                }
                Err(error) => log.push(format!("重注册未成功，继续离线安装：{error}")),
            }
        } else {
            log.push(format!("本机没有 App Installer，准备从 winget-cli {TAG} 官方 release 及国内镜像依次下载。"));
        }

        let dir = temp_work_dir()?;
        let proxy = local_proxy();
        if let Some(proxy) = &proxy {
            log.push(format!("检测到本机代理端口，GitHub 官方源不可达时会自动经 {proxy} 重试。"));
        }
        for &(name, expected) in PAYLOADS {
            // 本地文件哈希不符时 ensure_payload 只删文件并返回错误，这里重试一次真下载。
            let result = match ensure_payload(&dir, name, expected, proxy.as_deref()) {
                Ok(text) => Ok(text),
                Err(error) if error.contains("已删除待重下") => {
                    ensure_payload(&dir, name, expected, proxy.as_deref())
                }
                Err(error) => Err(error),
            };
            log.push(result?);
        }

        log.push("按依赖顺序注册 App Installer（缺 VCLibs / WinAppRuntime 会静默失败，逐项记录）。".into());
        let text = run_powershell(&register_script(&dir))
            .map_err(|error| format!("{}\n注册 App Installer 失败：{error}", log.join("\n")))?;
        log.push(text);

        match verify() {
            Some(version) => {
                log.push(format!("验证通过：winget {version} 已可用，环境页将恢复完整安装能力。"));
                // 315MB 只是一次性引导文件，成功后立刻清掉，不占用户磁盘。
                for &(name, _) in PAYLOADS {
                    let _ = fs::remove_file(dir.join(name));
                }
                let _ = fs::remove_dir_all(dir.join("deps"));
                Ok(log.join("\n"))
            }
            None => Err(format!(
                "{}\n\n注册流程已跑完但 winget 仍不可用。常见原因是系统版本低于 1809、\
                 组策略禁止旁加载，或需要注销/重启后别名才落地。\n\
                 不影响使用：安装与版本管理已自动走官方直连通道。",
                log.join("\n")
            )),
        }
    }
}

#[cfg(windows)]
pub use imp::repair;

#[cfg(not(windows))]
pub fn repair() -> Result<String, String> {
    Err("winget 修复只支持 Windows".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::imp::{download_attempts, register_script, sha256_file, DOWNLOAD_GUIDE, PAYLOADS};
    use std::path::Path;

    #[test]
    fn pinned_payloads_have_valid_sha256() {
        assert_eq!(PAYLOADS.len(), 3, "主体 + 依赖 zip + 许可证，缺一不可");
        for (name, digest) in PAYLOADS {
            assert_eq!(digest.len(), 64, "{name} 的 SHA256 必须是 64 位十六进制");
            assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let path = std::env::temp_dir().join(format!("nm-wingetboot-{}.bin", std::process::id()));
        std::fs::write(&path, b"abc").expect("写临时文件");
        let digest = sha256_file(&path).expect("应能算出哈希");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn download_attempts_prefer_official_then_proxy_then_mirrors() {
        let name = "DesktopAppInstaller_Dependencies.zip";
        let direct = download_attempts(name, None);
        assert_eq!(direct.len(), 4, "官方 + 三个国内镜像");
        assert!(direct[0]
            .0
            .starts_with("https://github.com/microsoft/winget-cli/releases/download/"));
        assert!(direct[0].1.is_none(), "官方源第一次先直连");
        assert!(direct[1..]
            .iter()
            .all(|(url, proxy)| url.starts_with("https://gh") && proxy.is_none()));

        let proxied = download_attempts(name, Some("http://127.0.0.1:7890"));
        assert_eq!(proxied.len(), 5, "内核在跑时多一次官方源走本机代理");
        assert_eq!(proxied[1].0, direct[0].0);
        assert_eq!(proxied[1].1.as_deref(), Some("http://127.0.0.1:7890"));
        assert!(proxied[2..].iter().all(|(_, proxy)| proxy.is_none()));
        assert!(proxied.iter().all(|(url, _)| url.ends_with(name)));
    }

    #[test]
    fn download_failure_guide_points_at_proxy_and_direct_channel() {
        assert!(DOWNLOAD_GUIDE.contains("导入节点配置"));
        assert!(DOWNLOAD_GUIDE.contains("开启接管"));
        assert!(DOWNLOAD_GUIDE.contains("官方直连"));
        assert!(DOWNLOAD_GUIDE.contains("不受影响"));
    }

    /// 联网自证：四个下载源返回的必须是同一个文件。镜像随时可能失效，
    /// 所以默认 ignore，手动跑 `cargo test ... -- --ignored --nocapture` 核对。
    #[test]
    #[ignore = "联网实测：所有下载源返回与官方 digest 一致的字节"]
    fn pinned_license_is_identical_across_download_sources() {
        let name = "e53e159d00e04f729cc2180cffd1c02e_License1.xml";
        let expected = PAYLOADS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("许可证应在钉住的清单里")
            .1;
        let dest = std::env::temp_dir().join(format!("nm-winget-license-{}.xml", std::process::id()));
        let part = dest.with_extension("part");
        for (url, proxy) in download_attempts(name, None) {
            let _ = std::fs::remove_file(&dest);
            let _ = std::fs::remove_file(&part);
            let mut cmd = crate::envdirect::hidden_command("curl.exe");
            cmd.args([
                "-sSL",
                "--fail",
                "--connect-timeout",
                "10",
                "--max-time",
                "60",
                "-A",
                "NetWorkManger-EnvDirect/1.0",
                "-o",
            ]);
            cmd.arg(&part);
            if let Some(proxy) = &proxy {
                cmd.args(["-x", proxy]);
            }
            cmd.arg(&url);
            let output = cmd.output().expect("应能调用 curl.exe");
            assert!(
                output.status.success(),
                "{url} 下载失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::fs::rename(&part, &dest).expect("应能重命名下载文件");
            assert_eq!(
                sha256_file(&dest).expect("应能算哈希"),
                expected,
                "{url} 内容与官方 digest 不一致"
            );
        }
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn register_script_installs_dependencies_before_bundle() {
        let script = register_script(Path::new("C:\\Temp\\nm"));
        let unzip = script.find("Expand-Archive").expect("应解压依赖 zip");
        let dep = script.find("DEP_OK").expect("应逐个注册依赖包");
        let bundle = script
            .find("Add-AppxPackage -Path $bundle")
            .expect("应注册 App Installer 主体");
        assert!(unzip < dep && dep < bundle, "依赖必须先于主体注册");
        assert!(script.contains("LicensePath"), "主体注册要带官方许可证");
        assert!(script.contains("arm64"), "依赖包要按 CPU 架构筛选");
    }
}
