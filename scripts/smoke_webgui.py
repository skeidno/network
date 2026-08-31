from __future__ import annotations

import argparse
from pathlib import Path
import time

from playwright.sync_api import expect, sync_playwright


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
        expect(page.locator("#page-title")).to_have_text("运行概览")
        expect(page.locator('[data-mode="SMART"]')).to_be_visible()

        page.locator('[data-page="servers"]').click()
        expect(page.locator("#page-title")).to_have_text("服务器部署")
        server_cards = page.locator(".ssh-server-card")
        if server_cards.count():
            first_card = server_cards.first
            deploy_button = first_card.locator('[data-ssh-action="deploy"]')
            assert deploy_button.is_visible()
            if first_card.locator('[data-ssh-action="copy"]:not([disabled])').count():
                assert "已部署" in first_card.inner_text()
                expect(deploy_button).to_contain_text("检查服务")

        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(args.output))
            page.set_viewport_size({"width": 820, "height": 760})
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-narrow.png")))

        page.set_viewport_size({"width": 1440, "height": 900})
        page.locator('[data-page="rules"]').click()
        expect(page.locator("#page-title")).to_have_text("分流规则")
        expect(page.locator(".fallback-rule-row")).to_contain_text("强制保底")
        expect(page.locator(".fallback-rule-row")).to_contain_text("始终位于规则末尾")
        if args.output:
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-rules.png")))

        page.locator('[data-page="nodes"]').click()
        expect(page.locator("#page-title")).to_have_text("代理与节点")
        page.locator('[data-node-tab="imported"]').click()
        expect(page.locator("#delete-error-nodes")).to_be_visible()
        if args.output:
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-nodes.png")))
        page.locator('[data-page="settings"]').click()
        expect(page.locator("#page-title")).to_have_text("设置")

        deadline = time.monotonic() + args.poll_seconds
        while time.monotonic() < deadline:
            page.locator("#header-refresh").click()
            page.wait_for_timeout(100)
        assert "后台连接中断" not in page.locator("#overview-status-detail").inner_text()

        browser.close()

    if errors:
        raise RuntimeError("\n".join(errors))
    print("WebGUI smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
