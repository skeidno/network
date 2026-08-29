from __future__ import annotations

import argparse
from pathlib import Path
import time

from playwright.sync_api import sync_playwright


def main() -> int:
    parser = argparse.ArgumentParser(description="Exercise a running Network Manager WebGUI")
    parser.add_argument("url")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--poll-seconds", type=int, default=8)
    args = parser.parse_args()

    errors: list[str] = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel="msedge", headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.on("pageerror", lambda error: errors.append(f"page: {error}"))
        page.on(
            "console",
            lambda message: errors.append(f"console: {message.text}")
            if message.type == "error"
            else None,
        )
        page.goto(args.url, wait_until="networkidle")
        page.locator("#page-title").wait_for(state="visible")
        assert page.locator("#page-title").inner_text() == "运行概览"

        page.locator('[data-page="servers"]').click()
        assert page.locator("#page-title").inner_text() == "SSH 服务器"
        page.locator("#add-ssh-server").click()
        page.locator("#modal-ssh-name").fill("Smoke Test Server")
        page.locator("#modal-ssh-host").fill("203.0.113.10")
        page.locator("#modal-ssh-username").fill("smoke")
        page.locator("#modal-actions .button.primary").click()
        page.get_by_text("Smoke Test Server", exact=True).wait_for()

        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(args.output))
            page.set_viewport_size({"width": 820, "height": 760})
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-narrow.png")))

        page.set_viewport_size({"width": 1440, "height": 900})
        page.locator('[data-page="nodes"]').click()
        assert page.locator("#page-title").inner_text() == "代理与节点"
        page.locator('[data-page="settings"]').click()
        assert page.locator("#page-title").inner_text() == "设置"

        deadline = time.monotonic() + args.poll_seconds
        while time.monotonic() < deadline:
            page.locator("#header-refresh").click()
            page.wait_for_timeout(100)
        assert "后台连接中断" not in page.locator("#overview-status-detail").inner_text()

        page.locator('[data-page="servers"]').click()
        page.locator('[data-ssh-action="delete"]').click()
        page.locator("#modal-actions .button.danger").click()
        page.get_by_text("暂无 SSH 服务器配置", exact=True).wait_for()
        browser.close()

    if errors:
        raise RuntimeError("\n".join(errors))
    print("WebGUI smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
