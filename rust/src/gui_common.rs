//! 界面层里与平台无关的那部分。
//!
//! 原生窗口（tao + wry）只在 Windows 上编，Linux 的形态是常驻服务。但 HTTP 命令层
//! （methods.rs）会往界面发动作、main 会记事件流，这些符号两边都得有，所以抽出来
//! 放这里，由 gui.rs（Windows 实现）和 gui_stub.rs（其他平台）各自 re-export。

use crate::tray::TrayCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Show,
    Hide,
    Minimize,
    Maximize,
    Close,
    Quit,
    /// 托盘线程发来的命令（打开界面 / 启停内核 / 退出）。
    Tray(TrayCommand),
}

pub struct Shell {
    pub url: String,
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub close_to_tray: bool,
    pub start_hidden: bool,
}

/// 事件循环内部出现异常（被 catch_unwind 收住）时的返回标记，
/// main 据此收掉内核后退出，而不是退回「开浏览器 + 等 Ctrl-C」的兜底路径。
pub const LOOP_CRASHED: &str = "event-loop-crashed";

/// 界面事件流水账（logs/gui.log）。
///
/// 「程序自己退了」这类问题最难查的地方是没有控制台，什么痕迹都不留。这里把
/// 关键分支记一笔，出问题时能直接看出是哪条退出路径被走到了。
pub fn trace(text: &str) {
    use std::io::Write;
    let path = crate::paths::logs_dir().join("gui.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 64 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "{now} {text}");
    }
}
