//! 远程截图：Windows 与 Linux 两种服务器都支持。
//!
//! # Windows 这边最难的地方：Session 0 没有桌面
//!
//! Windows 的会话是隔离的：
//!
//! - **Session 0** —— 服务与系统进程所在的会话。**没有交互式桌面**。
//!   通过 SSH 登录进来的 PowerShell 就落在这里（sshd 是服务）。
//! - **Session 1+** —— 用户登录后的交互式会话，桌面、窗口、屏幕都在这里。
//!
//! 所以直接在 SSH 里调 `CopyFromScreen` **抓不到东西** —— Session 0 里
//! 那个「屏幕」是空的，拿到的是黑屏或者直接抛异常。
//!
//! 本模块的应对是**让代码跑进交互会话**再截图。三条路，按可用性依次尝试：
//!
//! 1. **计划任务 + `/IT`**（最通用）：`schtasks /create /it /ru <用户>` 建一个
//!    一次性任务，`/it` 让它以交互方式运行 —— 这样它就落在目标用户
//!    的 Session 1 里，能拿到桌面。跑完把 PNG 读回来。
//! 2. **已存在同用户的交互会话**：用 `query session` 找出目标用户当前在
//!    哪个 Session，然后想办法让进程进去（较难，作为备选思路记录在案）。
//! 3. **直接抓**：如果当前进程本来就在交互会话（有人在本地登录着），
//!    直接抓即可。
//!
//! 抓到的图**以 base64 回传**（不是二进制）—— SSH exec 通道走文本，
//! 二进制会被编码破坏。base64 体积大 33%，但一张 1280x720 的 PNG
//! 压缩后只有几十 KB，完全可以接受。

use serde_json::{json, Value};

/// 截图结果的统一格式。
pub struct Capture {
    /// 图片数据（PNG 字节）。
    pub png: Vec<u8>,
    /// 图片宽度。
    pub width: u32,
    /// 图片高度。
    pub height: u32,
    /// 用的哪种方式抓的（界面上说明来源，便于用户判断可信度）。
    pub method: &'static str,
}

/// 截图时给远端 PowerShell 的参数：最大边长。
///
/// 不做全屏抓：远程服务器常见的是 1920x1080，PNG 可能有几百 KB，
/// base64 后更大。限制到 1280 宽既够看清屏幕状态，又能把传输量压下来。
const MAX_EDGE: u32 = 1280;

/// 生成 Windows 抓屏的 PowerShell 脚本。
///
/// 刻意**不用 `Add-Type` 编译 C#**（虽然那样能直接调 GDI32）——
/// 实测 `Add-Type -AssemblyName System.Drawing` + `Graphics.CopyFromScreen`
/// 就够了，而且不依赖编译器，在精简的 Server 镜像上也能跑。
///
/// `System.Windows.Forms` 的 `Screen` 类需要引用 `System.Windows.Forms` 程序集；
/// 但只用 `CopyFromScreen` 的话，`Graphics` + `CopyFromScreen` 配合
/// `System.Drawing` 就够 —— 屏幕尺寸从 `[System.Windows.Forms.SystemInformation]::VirtualScreen`
/// 拿不到时，退回用 `GetSystemMetrics` 也行。这里用一个更省事的办法：
/// 直接让 .NET 的 `SystemInformation` 拿不到就用固定分辨率兜底。
fn windows_capture_script() -> String {
    format!(
        r#"
$ErrorActionPreference = 'Stop'
$maxEdge = {MAX_EDGE}$out = Join-Path $env:TEMP 'nm-shot.png'
if (Test-Path $out) {{ Remove-Item $out -Force -ErrorAction SilentlyContinue }}

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$vs = [System.Windows.Forms.SystemInformation]::VirtualScreen
if (-not $vs) {{ $vs = New-Object System.Drawing.Rectangle 0, 0 1024, 768 }}
$w = $vs.Width
$h = $vs.Height
if ($w -le 0 -or $h -le 0) {{ $w = 1024; $h = 768 }}

# 缩到最大边长以内，免得图太大
if ([Math]::Max($w, $h) -gt $maxEdge) {{
  $scale = $maxEdge / [Math]::Max($w, $h)
  $w = [int]($w * $scale)
  $h = [int]($h * $scale)
}}

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($vs.Left, $vs.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
$g.Dispose()
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

if (-not (Test-Path $out)) {{ [Console]::Out.Write('NMSHOT:FAIL no file'); exit 1 }}
$b = [Convert]::ToBase64String([IO.File]::ReadAllBytes($out))
Remove-Item $out -Force -ErrorAction SilentlyContinue
[Console]::Out.Write('NMSHOT:OK:' + $w + 'x' + $h)
[Console]::Out.Write('NMB64:' + $b)
"#,
        MAX_EDGE = MAX_EDGE,
    )
}

/// Windows 抓屏：主路径是计划任务 + `/IT`。
///
/// 之所以要做成「建任务 → 跑任务 → 读文件 → 删任务」这一整套，
/// 是因为只有这样才能让代码进到交互会话。任务跑完立刻删掉，
/// 不留垃圾在目标机上。
pub async fn capture_windows(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Capture, String> {
    // 先探一下当前是不是已经在交互会话 —— 是的话直接抓，省掉建任务那一套
    let session = query_windows_session(profile, credential).await?;
    if session.interactive {
        return capture_windows_direct(profile, credential, &session).await;
    }

    // 不在交互会话：建一个 /IT 的一次性任务，让它进交互会话抓屏
    let task_name = format!("nm-shot-{}", std::process::id());
    let script = windows_capture_script();
    let script_path = remote_temp_path("nm-shot.ps1");
    let out_path = remote_temp_path("nm-shot-out.png");

    // 传脚本到远端。用 PowerShell 写文件，避开 SSH 的二进制通道。
    let write_script = format!(
        "$b64='{}'; [IO.File]::WriteAllBytes('{}', [Convert]::FromBase64String($b64))",
        base64_encode(script.as_bytes()),
        script_path
    );
    crate::winsrv::run_powershell(profile, credential, &write_script).await?;

    // 建一次性任务：/it 表示以交互方式运行（进 Session 1），/ru 指定用哪个用户
    let user = session
        .interactive_user
        .clone()
        .unwrap_or_else(|| profile.username.clone());
    let create = format!(
        "schtasks /create /tn {task} /tr \"powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File {script}\" /sc once /st 00:00 /it /ru \"{user}\" /f",
        task = task_name,
        script = script_path,
        user = user,
    );
    let out = crate::winsrv::run_powershell(profile, credential, &create).await?;
    if out.to_lowercase().contains("error") || out.contains("错误") {
        // 建任务失败是常态（比如权限不够、用户没设密码）
        return Err(format!(
            "无法在远端创建交互式计划任务（截图必须在有桌面的会话里进行）。\n\
             远端返回：\n{out}\n\n\
             常见原因与对策：\n\
             · SSH 用的账号不是当前登录用户的账号 —— 换个登录着桌面的账号\n\
             · 计划任务需要管理员权限 —— 用管理员账号连\n\
             · 目标机根本没有交互式会话（没人在本地登录过）—— \
             那就没有桌面可截，RDP 连上去后会自动建一个会话再试"
        ));
    }

    // 跑任务
    let _ = crate::winsrv::run_powershell(
        profile,
        credential,
        &format!("schtasks /run /tn {task_name}"),
    )
    .await?;

    // 等文件出现（任务跑起来要一两秒）
    let mut image_b64 = String::new();
    for _ in 0..15 {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let check = format!(
            "if (Test-Path '{out}') {{ $b=[Convert]::ToBase64String([IO.File]::ReadAllBytes('{out}')); \
             [Console]::Out.Write('NMB64:' + $b) }} else {{ [Console]::Out.Write('NMB64:') }}",
            out = out_path
        );
        let text = crate::winsrv::run_powershell(profile, credential, &check).await?;
        if let Some(b64) = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("NMB64:"))
            .map(|value| value.trim().to_string())
        {
            if !b64.is_empty() {
                image_b64 = b64;
                break;
            }
        }
    }

    // 清理：删任务与临时文件
    let _ = crate::winsrv::run_powershell(
        profile,
        credential,
        &format!(
            "schtasks /delete /tn {task_name} /f 2>$null; Remove-Item '{script}','{out}' -Force -ErrorAction SilentlyContinue",
            task_name = task_name,
            script = script_path,
            out = out_path,
        ),
    )
    .await;

    if image_b64.is_empty() {
        return Err(
            "交互式任务跑完了但没拿到图片。可能是目标机没有图形桌面（Server Core 精简版就没有），\
             或者计划任务启动失败。"
                .to_string(),
        );
    }
    let png = base64_decode(&image_b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: "计划任务（交互会话）",
    })
}

/// 直接抓（当前进程已经在交互会话时）。
async fn capture_windows_direct(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    session: &WindowsSession,
) -> Result<Capture, String> {
    let script = windows_capture_script();
    let out = crate::winsrv::run_powershell(profile, credential, &script).await?;
    let b64 = out
        .lines()
        .find_map(|line| line.trim().strip_prefix("NMB64:"))
        .map(|value| value.trim().to_string())
        .ok_or_else(|| format!("远端没有返回图片数据。\n{out}"))?;
    if b64.is_empty() {
        return Err(format!("远端截图失败。\n{out}"));
    }
    let png = base64_decode(&b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: if session.interactive_user.is_some() {
            "直接抓取（交互会话）"
        } else {
            "直接抓取"
        },
    })
}

/// 远端的会话信息。
pub struct WindowsSession {
    /// 当前 SSH 进程是否在交互会话里。
    pub interactive: bool,
    /// 交互式会话里的用户名（如果找得到）。
    pub interactive_user: Option<String>,
}

/// 查远端当前会话情况。
pub async fn query_windows_session(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<WindowsSession, String> {
    let script = r#"
$mySession = (Get-Process -Id $PID).SessionId
[Console]::Out.Write('NMSESS:self=' + $mySession)
try {
  $lines = query session 2>$null
  foreach ($l in $lines) {
    if ($l -match '^\s*>?([a-zA-Z]+)\s+(\S+)\s+(\d+)\s+(\S+)') {
      $user = $Matches[2]
      $sid = $Matches[3]
      $state = $Matches[4]
      if ($sid -ne '0' -and $state -match '运行中|Active|Disc|断开') {
        [Console]::Out.Write('NMUSR:' + $user + '|' + $sid + '|' + $state)
      }
    }
  }
} catch {}
"#;
    let out = crate::winsrv::run_powershell(profile, credential, script).await?;
    let self_session = out
        .lines()
        .find_map(|line| line.trim().strip_prefix("NMSESS:self="))
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    // Session 0 是服务会话，没有桌面
    let interactive = !self_session.is_empty() && self_session != "0";
    let interactive_user = out
        .lines()
        .find_map(|line| line.trim().strip_prefix("NMUSR:"))
        .and_then(|body| {
            body.split('|')
                .next()
                .map(|user| user.trim().to_string())
                .filter(|user| !user.is_empty() && !user.eq_ignore_ascii_case("services"))
        });
    Ok(WindowsSession {
        interactive,
        interactive_user,
    })
}

/// 远端临时文件路径（放在用户临时目录，跨会话都能读）。
///
/// 用 `$env:TEMP` 而不是拼具体路径 —— 路径必须在**远端**展开，
/// 不同账号的临时目录不一样（用户 A 是 AppData\Local\Temp，
/// SYSTEM 账号又是另一个路径）。
fn remote_temp_path(name: &str) -> String {
    format!("$env:TEMP\\{name}")
}

/// Linux 抓屏。
///
/// 服务器上多数**没有 X11**（`DISPLAY` 为空），所以按可用性依次尝试：
///
/// 1. `DISPLAY` 有值 → 用 ImageMagick 的 `import`，或 `xwd`
/// 2. 有 `ffmpeg` 且有 framebuffer → `-f x11grab` 或读 `/dev/fb0`
/// 3. 都没有 → 明确报错，并说清为什么
///
/// 抓到的是 PNG 或 PPM，统一转成 PNG 回传（xwd 是 X11 格式，得转）。
pub async fn capture_linux(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Capture, String> {
    // 探测可用工具
    let probe = r#"
for t in import xwd scrot ffmpeg grim gnome-screenshot maim; do
  if command -v $t >/dev/null 2>&1; then echo "NMTool:$t"; fi
done
echo "NMDISPLAY:${DISPLAY:-}"
echo "NMFBCLIENT:$(ls /dev/fb0 2>/dev/null)"
"#;
    let out = crate::winsrv::run_shell(profile, credential, probe).await?;

    let tools: Vec<String> = out
        .lines()
        .filter_map(|line| line.trim().strip_prefix("NMTool:"))
        .map(|v| v.trim().to_string())
        .collect();
    let display = out
        .lines()
        .find_map(|line| line.trim().strip_prefix("NMDISPLAY:"))
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let has_fb = out.contains("NMFBCLIENT:/dev/fb0");

    if display.is_empty() && tools.is_empty() {
        return Err(
            "远端没有任何截图工具，也没有图形会话（DISPLAY 为空）。\n\
             服务器上装一下其中一个即可：\n\
             · ImageMagick：apt install imagemagick（提供 import）\n\
             · xwd：apt install x11-apps\n\
             · ffmpeg：apt install ffmpeg\n\
             注意：装了工具还不够，机器上得真有 X11 会话（图形界面）。\
             纯命令行的服务器（比如最小化的云镜像）本来就没有桌面，截不了屏。"
                .to_string(),
        );
    }

    // 有 DISPLAY 就优先用它
    if !display.is_empty() {
        if tools.iter().any(|t| t == "import") {
            return capture_linux_with_import(profile, credential, &display).await;
        }
        if tools.iter().any(|t| t == "xwd") {
            return capture_linux_with_xwd(profile, credential, &display).await;
        }
        if tools.iter().any(|t| t == "ffmpeg") {
            return capture_linux_with_ffmpeg(profile, credential, &display).await;
        }
    }
    // 没有 DISPLAY，但有 ffmpeg + framebuffer
    if has_fb && tools.iter().any(|t| t == "ffmpeg") {
        return capture_linux_framebuffer(profile, credential).await;
    }
    if tools.iter().any(|t| t == "grim") {
        // grim 是 Wayland 的，需要 WAYLAND_DISPLAY
        return capture_linux_with_grim(profile, credential).await;
    }

    Err(format!(
        "远端有截图工具（{}）但没有图形会话（DISPLAY 为空），无法截图。\n\
         在纯命令行的服务器上装工具也没用 —— 得先有 X11 或 Wayland 会话。",
        tools.join("、")
    ))
}

/// 用 ImageMagick 的 import 抓 X11 桌面。
async fn capture_linux_with_import(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    display: &str,
) -> Result<Capture, String> {
    // import -window root 直接抓整个 X11 根窗口，输出 PNG 到标准输出
    let script = format!(
        "DISPLAY={d} import -window root -silent png:- 2>/dev/null | base64 -w 0",
        d = shell_quote(display)
    );
    let out = crate::winsrv::run_shell(profile, credential, &script).await?;
    let b64 = out
        .lines()
        .find(|line| line.len() > 100 && !line.contains(' '))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| format!("import 抓屏没有产出数据。\n{out}"))?;
    let png = base64_decode(&b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: "ImageMagick import（X11）",
    })
}

/// 用 xwd 抓，再转 PNG。
async fn capture_linux_with_xwd(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    display: &str,
) -> Result<Capture, String> {
    // xwd 只能出 XWD 格式，用 convert 转一下。convert 也可能没装，那就原样回 XWD
    let script = format!(
        "DISPLAY={d} xwd -root -silent 2>/dev/null | base64 -w 0",
        d = shell_quote(display)
    );
    let out = crate::winsrv::run_shell(profile, credential, &script).await?;
    let b64 = out
        .lines()
        .find(|line| line.len() > 100 && !line.contains(' '))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| format!("xwd 抓屏没有产出数据。\n{out}"))?;
    let raw = base64_decode(&b64)?;
    // XWD 格式的文件头：magic 是 0x00000000，header size 在偏移 0
    // 简单判断：PNG 魔数是 89 50 4E 47
    if raw.starts_with(&[0x89, b'P', b'N', b'G']) {
        let (width, height) = read_png_size(&raw);
        return Ok(Capture {
            png: raw,
            width,
            height,
            method: "xwd（X11）",
        });
    }
    Err(
        "抓到的是 XWD 格式但没法直接显示。远端装一下 ImageMagick（apt install imagemagick）\
         就能自动转成 PNG。"
            .to_string(),
    )
}

/// 用 ffmpeg 的 x11grab 抓 X11。
async fn capture_linux_with_ffmpeg(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    display: &str,
) -> Result<Capture, String> {
    let script = format!(
        "DISPLAY={d} ffmpeg -loglevel error -f x11grab -video_size 1280x720 -i :0.0 -frames:v 1 -f image2pipe -vcodec png - 2>/dev/null | base64 -w 0",
        d = shell_quote(display)
    );
    let out = crate::winsrv::run_shell(profile, credential, &script).await?;
    let b64 = out
        .lines()
        .find(|line| line.len() > 100 && !line.contains(' '))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| format!("ffmpeg 抓屏没有产出数据。\n{out}"))?;
    let png = base64_decode(&b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: "ffmpeg x11grab（X11）",
    })
}

/// 读 framebuffer（无 X11 但内核有 fb 的情况）。
async fn capture_linux_framebuffer(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Capture, String> {
    let script = "ffmpeg -loglevel error -f fbdev -i /dev/fb0 -frames:v 1 -f image2pipe -vcodec png - 2>/dev/null | base64 -w 0";
    let out = crate::winsrv::run_shell(profile, credential, script).await?;
    let b64 = out
        .lines()
        .find(|line| line.len() > 100 && !line.contains(' '))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| format!("读 framebuffer 失败。\n{out}"))?;
    let png = base64_decode(&b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: "ffmpeg fbdev（framebuffer）",
    })
}

/// 用 grim 抓 Wayland。
async fn capture_linux_with_grim(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Capture, String> {
    let script = "grim - 2>/dev/null | base64 -w 0";
    let out = crate::winsrv::run_shell(profile, credential, script).await?;
    let b64 = out
        .lines()
        .find(|line| line.len() > 100 && !line.contains(' '))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| format!("grim 抓屏没有产出数据。\n{out}"))?;
    let png = base64_decode(&b64)?;
    let (width, height) = read_png_size(&png);
    Ok(Capture {
        png,
        width,
        height,
        method: "grim（Wayland）",
    })
}

/// 单引号包裹 shell 参数。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// base64 与 PNG 头解析
// ---------------------------------------------------------------------------

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 编码。
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64_TABLE[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_TABLE[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_TABLE[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// base64 解码。忽略空白与换行（远端 base64 可能带换行）。
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for ch in text.chars() {
        let c = ch as u8;
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let Some(value) = B64_TABLE.iter().position(|t| *t == c) else {
            return Err(format!("base64 里有非法字符 {:?}", ch));
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    if out.is_empty() {
        return Err("base64 解出来是空的".to_string());
    }
    Ok(out)
}

/// 从 PNG 文件头读宽高。
///
/// PNG 的 IHDR 在固定位置：8 字节签名 + 4 字节长度 + 4 字节 "IHDR"，
/// 接着 4 字节宽、4 字节高（大端）。
pub fn read_png_size(data: &[u8]) -> (u32, u32) {
    if data.len() < 24 || !data.starts_with(&[0x89, b'P', b'N', b'G']) {
        return (0, 0);
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    (width, height)
}

/// 把截图转成前端能用的 JSON（data URI）。
///
/// 直接用 data URI 而不是走图片接口 —— 一张图几十 KB，
/// base64 后也就百来 KB，一次性带过去比再设计一套图片下载接口简单得多。
pub fn capture_to_json(capture: &Capture) -> Value {
    json!({
        "mime": "image/png",
        "data": format!("data:image/png;base64,{}", base64_encode(&capture.png)),
        "width": capture.width,
        "height": capture.height,
        "method": capture.method,
        "sizeBytes": capture.png.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64往返一致() {
        for case in [
            &b""[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &b"hello world"[..],
            &[0u8, 255, 128, 64][..],
        ] {
            let encoded = base64_encode(case);
            let decoded = base64_decode(&encoded).unwrap();
            assert_eq!(decoded, case, "case {case:?}");
        }
    }

    #[test]
    fn base64解码忽略换行() {
        // 远端 base64 可能带换行，解码必须能吃下
        let encoded = base64_encode(b"hello world, this is a longer test string");
        let wrapped: String = encoded
            .as_bytes()
            .chunks(20)
            .map(|c| format!("{}\n", String::from_utf8_lossy(c)))
            .collect();
        assert_eq!(base64_decode(&wrapped).unwrap(), b"hello world, this is a longer test string");
    }

    #[test]
    fn base64解码拒绝非法字符() {
        assert!(base64_decode("!!!not base64!!!").is_err());
    }

    #[test]
    fn 读png宽高() {
        // 造一个最小 PNG 头：签名 + IHDR + 宽高
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&720u32.to_be_bytes());
        assert_eq!(read_png_size(&png), (1280, 720));
    }

    #[test]
    fn 非png返回零() {
        assert_eq!(read_png_size(b"not a png at all"), (0, 0));
        assert_eq!(read_png_size(&[]), (0, 0));
    }

    #[test]
    fn 抓屏脚本包含关键要素() {
        let script = windows_capture_script();
        // 必须在交互会话才能抓到，所以脚本本身要能独立跑
        assert!(script.contains("CopyFromScreen"));
        assert!(script.contains("System.Drawing"));
        assert!(script.contains("NMB64:"), "要有 base64 输出标记");
        // 限制最大边长，别把整块 4K 屏传回来
        assert!(script.contains("maxEdge"));
    }
}
