//! 环境页长任务的真实进度。
//!
//! 装 / 卸一个 winget 包要几十秒到几分钟。以前界面只知道「busy」，用户对着
//! 一个转圈按钮完全不知道进行到第几项、卡在哪一步。这里维护一份任务状态，
//! 由后端在每个动作真正开始、真正结束时推进，`/api/state` 原样带出去。
//!
//! 刻意不放进 `AppState`：长动作跑在 `shared` 锁外（见 methods.rs），
//! 进度更新若也要拿那把锁，就会跟状态轮询抢锁；用独立的全局锁，临界区只有几次赋值，
//! 既不影响轮询，刷新页面后任务进度也还在。
//!
//! 进度是「第 N/M 项 + 当前阶段 + 每个已结束项的真实结果」，不是动画假百分比：
//! winget 本身不给逐百分比进度，硬编一个 37% 出来反而是骗人。

use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct ItemLog {
    id: String,
    ok: bool,
    detail: String,
}

#[derive(Debug)]
struct Task {
    label: String,
    items: Vec<String>,
    current: usize,
    /// running / done / failed
    status: &'static str,
    phase: String,
    started_at_ms: u64,
    started: Instant,
    failures: usize,
    logs: Vec<ItemLog>,
}

impl Task {
    fn new(label: &str, items: Vec<String>) -> Self {
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        Self {
            label: label.to_string(),
            items,
            current: 0,
            status: "running",
            phase: "准备中".to_string(),
            started_at_ms,
            started: Instant::now(),
            failures: 0,
            logs: Vec::new(),
        }
    }

    fn finish(&mut self) {
        if self.status != "running" {
            return;
        }
        self.status = if self.failures > 0 { "failed" } else { "done" };
        self.phase = if self.failures > 0 {
            format!("结束，{} 项失败", self.failures)
        } else {
            "全部完成".to_string()
        };
    }
}

static TASK: OnceLock<Mutex<Option<Task>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Task>> {
    TASK.get_or_init(|| Mutex::new(None))
}

fn update<R>(f: impl FnOnce(&mut Option<Task>) -> R) -> R {
    let mut guard = slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// 批量任务开始。前端在逐项调用动作之前先报一次总数，
/// 这样单项动作共用同一份任务，计数才是真的「第 3 项 / 共 5 项」。
pub fn begin(label: &str, items: Vec<String>) {
    let items: Vec<String> = items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();
    update(|task| *task = Some(Task::new(label, items)));
}

/// 单项动作：若没有进行中的批量任务包含它，就自己开一个一项的任务。
pub fn ensure_item(label: &str, id: &str) {
    update(|task| {
        let covered = task
            .as_ref()
            .map(|current| current.status == "running" && current.items.iter().any(|i| i == id))
            .unwrap_or(false);
        if !covered {
            *task = Some(Task::new(label, vec![id.to_string()]));
        }
    });
}

/// 某项动作真正开始。
pub fn begin_item(id: &str) {
    update(|task| {
        if let Some(current) = task.as_mut() {
            if let Some(index) = current.items.iter().position(|item| item == id) {
                current.current = index + 1;
            }
            current.status = "running";
            current.phase = format!("正在处理 {id}");
        }
    });
}

/// 换阶段文案（卸载时用于「官方卸载 → 清理残留 → 复核」这类多步动作）。
pub fn set_phase(phase: &str) {
    update(|task| {
        if let Some(current) = task.as_mut() {
            current.phase = phase.to_string();
        }
    });
}

/// 某项动作结束，记下真实成败与首行输出。
pub fn end_item(id: &str, ok: bool, detail: &str) {
    let short: String = detail
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    update(|task| {
        if let Some(current) = task.as_mut() {
            if !ok {
                current.failures += 1;
            }
            current.logs.push(ItemLog {
                id: id.to_string(),
                ok,
                detail: short,
            });
            if current.current >= current.items.len() {
                current.finish();
            }
        }
    });
}

/// 批量循环提前中断（前端某项抛错、用户关页面等）时收尾，
/// 否则任务会永远停在 running，界面上一直显示「进行中」。
pub fn finish() {
    update(|task| {
        if let Some(current) = task.as_mut() {
            current.finish();
        }
    });
}

pub fn clear() {
    update(|task| *task = None);
}

pub fn snapshot() -> Value {
    update(|task| match task.as_ref() {
        None => Value::Null,
        Some(current) => {
            let total = current.items.len();
            let percent = if total == 0 {
                0
            } else {
                ((current.current as f64 / total as f64) * 100.0).round() as u32
            };
            json!({
                "active": current.status == "running",
                "status": current.status,
                "label": current.label,
                "current": current.current,
                "total": total,
                "percent": percent,
                "item": current.items.get(current.current.saturating_sub(1)).cloned().unwrap_or_default(),
                "phase": current.phase,
                "startedAt": current.started_at_ms,
                "elapsedMs": current.started.elapsed().as_millis() as u64,
                "failures": current.failures,
                "logs": current.logs.iter().map(|entry| json!({
                    "id": entry.id,
                    "ok": entry.ok,
                    "detail": entry.detail,
                })).collect::<Vec<_>>(),
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 一个测试里跑完整流程，避免并行测试互相踩同一份全局任务状态。
    #[test]
    fn batch_progress_counts_real_items_and_keeps_failure() {
        clear();
        begin("一键部署", vec!["go".into(), "python".into()]);
        let first = snapshot();
        assert_eq!(first["current"], 0);
        assert_eq!(first["total"], 2);
        assert_eq!(first["active"], true);

        begin_item("go");
        assert_eq!(snapshot()["current"], 1);
        end_item("go", true, "Go 安装完成\n细节");
        // 两项任务，第一项结束不能提前标完成
        assert_eq!(snapshot()["active"], true);

        begin_item("python");
        assert_eq!(snapshot()["current"], 2);
        end_item("python", false, "winget 退出码 1");
        let done = snapshot();
        assert_eq!(done["active"], false);
        assert_eq!(done["status"], "failed");
        assert_eq!(done["percent"], 100);
        assert_eq!(done["failures"], 1);
        assert_eq!(done["logs"].as_array().map(Vec::len), Some(2));

        // 单项动作另起一份任务，且第一项结束就算完成
        ensure_item("卸载", "go");
        begin_item("go");
        end_item("go", true, "已卸载");
        let single = snapshot();
        assert_eq!(single["status"], "done");
        assert_eq!(single["total"], 1);
        clear();
        assert!(snapshot().is_null());
    }
}
