//! 跨端口的单实例锁（按进程判重，不按端口）。
//!
//! 旧实现只 GET `host:port` 判活：显式带一个不同的 `--port` 就能再起一个进程，
//! 而两个进程共用同一份 settings.json、同一个 mihomo 内核与路由表，实测会把
//! 对方的连接重置打挂。正常用户不会这么干，但开机自启 + 手动双击、或调试时
//! 带端口启动，都会踩到。
//!
//! 现在在共享数据目录里放一份 `instance.lock`（pid / host / port / 进程名），
//! 判重依据是「pid 对应的进程是否还活着且名字对得上」—— 进程被强杀后锁会
//! 残留，靠这一步自动清理，不需要用户手动删文件。`create_new` 保证两个进程
//! 同时启动也只有一个能拿到锁，端口不再是判重维度。
//!
//! 锁按数据目录隔离：`NETWORK_MANAGER_DATA_DIR` 不同是刻意开的隔离实例
//! （CI 冒烟、自证验证脚本都靠它），互不干扰；同一份配置永远只允许一个进程。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    pid: u32,
    host: String,
    port: u16,
    /// 写下这条记录时的可执行文件名。校验时拿它跟 pid 的实际进程名比对，
    /// 防止 pid 被系统复用给别的程序后误判成「已有实例」。
    exe: String,
    version: String,
}

impl Record {
    fn mine(host: &str, port: u16) -> Self {
        Self {
            pid: std::process::id(),
            host: host.to_string(),
            port,
            exe: exe_name(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// 已经在跑的那个实例，用来把窗口唤醒到前台。
#[derive(Debug, Clone)]
pub struct Owner {
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

pub enum Claim {
    /// 自己拿到了唯一实例资格；Guard 活着期间锁归自己。
    Acquired(Guard),
    /// 已经有实例在跑，这一趟应该唤醒它然后退出。
    Existing(Owner),
}

/// 退出时只删「确实还属于自己的」锁，避免误删后来者写的记录。
pub struct Guard {
    path: PathBuf,
    mine: Record,
    active: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(record) = read_record(&self.path) {
            if record.pid == self.mine.pid
                && record.host == self.mine.host
                && record.port == self.mine.port
                && record.exe == self.mine.exe
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn lock_path() -> PathBuf {
    crate::paths::app_data_dir().join("instance.lock")
}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "network-manager-rs".to_string())
}

fn read_record(path: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Record>(&text).ok()
}

/// pid 还活着，而且确实是写锁时那个程序（不是被复用的别的进程）。
fn process_matches(pid: u32, name: &str) -> bool {
    if pid == 0 || name.is_empty() {
        return false;
    }
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let target = Pid::from_u32(pid);
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory()),
    );
    system.refresh_processes(ProcessesToUpdate::Some(&[target]));
    system
        .process(target)
        .map(|process| process.name().eq_ignore_ascii_case(name))
        .unwrap_or(false)
}

/// 尝试取得唯一实例资格。`allow_multi` 是自证/调试用的显式豁免：既不检查
/// 也不写锁，让同一份数据目录下可以临时并行起多个进程。
pub fn claim(host: &str, port: u16, allow_multi: bool) -> Claim {
    claim_at(&lock_path(), host, port, allow_multi)
}

fn claim_at(path: &Path, host: &str, port: u16, allow_multi: bool) -> Claim {
    let mine = Record::mine(host, port);
    if allow_multi {
        return Claim::Acquired(Guard {
            path: path.to_path_buf(),
            mine,
            active: false,
        });
    }

    // 竞态窗口：两个进程同时看到陈旧锁、同时删除、同时 create_new，
    // 输的那个回到循环开头就会读到赢家的记录并让位。
    for attempt in 0..4 {
        if path.exists() {
            // create_new 是先建空文件再写内容，另一个进程可能刚好读到半截。
            // 空文件不能当成「损坏锁」直接删，否则会把刚赢的那个实例的锁抹掉。
            let raw = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(_) => {
                    eprintln!("单实例锁不可读（{}），退回端口探测", path.display());
                    return Claim::Acquired(Guard {
                        path: path.to_path_buf(),
                        mine,
                        active: false,
                    });
                }
            };
            match serde_json::from_str::<Record>(&raw) {
                Ok(record) if record.pid != mine.pid && process_matches(record.pid, &record.exe) => {
                    return Claim::Existing(Owner {
                        pid: record.pid,
                        host: record.host,
                        port: record.port,
                    });
                }
                Ok(_) => {} // 进程已不在 / pid 被复用：陈旧锁
                Err(_) if raw.trim().is_empty() && attempt < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    continue;
                }
                Err(_) => {} // 真正写坏了的 JSON
            }
            let _ = std::fs::remove_file(path);
        }

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                let payload = serde_json::to_string(&mine).unwrap_or_default();
                if file.write_all(payload.as_bytes()).is_err() {
                    // 写不完整的锁没有判重价值，删掉，别留下半截 JSON。
                    let _ = std::fs::remove_file(path);
                }
                return Claim::Acquired(Guard {
                    path: path.to_path_buf(),
                    mine,
                    active: true,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // 数据目录只读 / 权限异常：不能因为一把锁就把程序锁死，
                // 退回原来「按端口探测」的行为，由 main 里的兜底处理。
                eprintln!("单实例锁不可用（{}），退回端口探测", path.display());
                return Claim::Acquired(Guard {
                    path: path.to_path_buf(),
                    mine,
                    active: false,
                });
            }
        }
    }

    // 四次都没拿到锁：说明有别的进程在抢，宁可让位也不要双开抢内核。
    Claim::Existing(Owner {
        pid: 0,
        host: host.to_string(),
        port,
    })
}

/// 唤醒链路整个失败时（比如已有实例设了 WebGUI 密码，API 鉴权过不去，
/// 窗口也没找到）至少给用户一句人话，而不是「双击了没反应」。
#[cfg(windows)]
pub fn notify_second_launch() {
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
    };
    unsafe {
        MessageBoxW(
            Some(HWND::default()),
            w!("Network Manager 已经在运行，已为你切换到现有窗口。\n\n同一份配置与内核只允许一个程序实例；若确实需要再开一个，可用 --allow-multi-instance 启动。"),
            w!("Network Manager"),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
pub fn notify_second_launch() {
    eprintln!("Network Manager 已经在运行；同一份配置只允许一个实例");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// 找一个确实活着、且 pid 不是测试进程自己的进程来冒充「已有实例」。
    /// 用父进程（跑 cargo test 的那个）最省事，也不用额外起子进程。
    fn other_live_process() -> (u32, String) {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let mut system = System::new_all();
        system.refresh_processes(ProcessesToUpdate::All);
        let self_pid = Pid::from_u32(std::process::id());
        let parent = system
            .process(self_pid)
            .and_then(|process| process.parent())
            .expect("测试进程的父进程应当还在");
        let name = system
            .process(parent)
            .map(|process| process.name().to_string_lossy().into_owned())
            .expect("父进程信息应可查询");
        (parent.as_u32(), name)
    }

    fn temp_lock(label: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "nm-instance-test-{}-{}-{}-{}.lock",
            label,
            std::process::id(),
            seq,
            chrono::Utc::now().timestamp_millis()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let path = temp_lock("stale");
        let stale = Record {
            pid: u32::MAX - 1,
            host: "127.0.0.1".into(),
            port: 19091,
            exe: "definitely-not-running.exe".into(),
            version: "0".into(),
        };
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        let guard = match claim_at(&path, "127.0.0.1", 19092, false) {
            Claim::Acquired(guard) => guard,
            Claim::Existing(_) => panic!("陈旧锁不该被当成已有实例"),
        };
        let record = read_record(&path).expect("重新写出的锁应可解析");
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.port, 19092);
        drop(guard);
        assert!(!path.exists(), "正常退出应清掉自己的锁");
    }

    #[test]
    fn live_lock_blocks_second_instance() {
        let path = temp_lock("live");
        let (other_pid, other_name) = other_live_process();
        let owner = Record {
            pid: other_pid,
            host: "127.0.0.1".into(),
            port: 19091,
            exe: other_name,
            version: "0".into(),
        };
        std::fs::write(&path, serde_json::to_string(&owner).unwrap()).unwrap();

        match claim_at(&path, "127.0.0.1", 19092, false) {
            Claim::Existing(found) => {
                assert_eq!(found.pid, owner.pid);
                assert_eq!(found.port, 19091, "应按锁里的端口唤醒，不是新进程的端口");
                assert_eq!(found.host, "127.0.0.1");
            }
            Claim::Acquired(_) => panic!("活跃的实例锁不该被抢走"),
        }
        // 第二个实例退出时不能把别人的锁删掉。
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupted_lock_is_reclaimed() {
        let path = temp_lock("corrupt");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            claim_at(&path, "127.0.0.1", 9091, false),
            Claim::Acquired(_)
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn guard_does_not_remove_someone_elses_lock() {
        let path = temp_lock("handover");
        let guard = match claim_at(&path, "127.0.0.1", 9091, false) {
            Claim::Acquired(guard) => guard,
            Claim::Existing(_) => panic!("空目录应能拿到锁"),
        };
        let other = Record {
            pid: u32::MAX - 2,
            host: "127.0.0.1".into(),
            port: 9092,
            exe: "other.exe".into(),
            version: "0".into(),
        };
        std::fs::write(&path, serde_json::to_string(&other).unwrap()).unwrap();
        drop(guard);
        assert!(path.exists(), "锁已经易主，旧 Guard 不该删它");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn half_written_lock_waits_for_the_writer() {
        // 模拟两个进程同时启动：赢家刚 create_new 出空文件，还没写完 JSON。
        // 输家不能把这份半截锁当垃圾删掉，否则两边都会认为自己拿到了锁。
        use std::time::Duration;
        let path = temp_lock("race");
        std::fs::write(&path, b"").unwrap();
        let writer_path = path.clone();
        let (other_pid, other_name) = other_live_process();
        let owner = Record {
            pid: other_pid,
            host: "127.0.0.1".into(),
            port: 19091,
            exe: other_name,
            version: "0".into(),
        };
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(&writer_path, serde_json::to_string(&owner).unwrap()).unwrap();
        });
        let result = claim_at(&path, "127.0.0.1", 19092, false);
        writer.join().unwrap();
        assert!(
            matches!(result, Claim::Existing(_)),
            "应等到写锁进程写完，然后让位给已有实例"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn allow_multi_does_not_touch_lock() {
        let path = temp_lock("multi");
        let owner = Record::mine("127.0.0.1", 19091);
        std::fs::write(&path, serde_json::to_string(&owner).unwrap()).unwrap();
        // 活跃锁也不该被豁免实例碰；这里自己 pid 的锁会被当成残留，正好验证
        // 豁免实例既不判重也不改写文件内容。
        assert!(matches!(
            claim_at(&path, "127.0.0.1", 19092, true),
            Claim::Acquired(_)
        ));
        assert_eq!(read_record(&path).unwrap().port, 19091, "豁免实例不该改锁");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn process_check_rejects_unknown_pid_and_accepts_self() {
        assert!(!process_matches(u32::MAX - 1, "anything.exe"));
        assert!(!process_matches(std::process::id(), "not-our-program.exe"));
        assert!(process_matches(std::process::id(), &exe_name()));
    }
}
