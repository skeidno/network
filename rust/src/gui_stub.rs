//! 非 Windows 平台的界面层：没有原生窗口，界面动作全部落空。
//!
//! Linux 上程序以 systemd 常驻服务的形式跑，管理页面由浏览器访问 HTTP 端口打开，
//! 不存在「隐藏/显示窗口」这类动作。命令层照旧调用，这里收下并忽略。

pub use crate::gui_common::*;

/// 没有事件循环可以投递，忽略即可。
pub fn send(_event: AppEvent) {}

/// 服务器场景没有文件选择对话框，需要私钥就请在页面上填路径。
pub fn pick_ssh_key_blocking() -> Option<String> {
    None
}

/// 同理：文件快传的本机路径在服务端场景只能手输（浏览器那边选不了
/// 服务器上的路径），返回 None 让前端保留用户填的内容。
pub fn pick_local_path_blocking(_select_directory: bool) -> Option<String> {
    None
}
