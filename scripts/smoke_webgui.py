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
    parser.add_argument("--username")
    parser.add_argument("--password")
    parser.add_argument("--fail-first-state-requests", type=int, default=0)
    parser.add_argument("--test-close-to-tray", action="store_true")
    args = parser.parse_args()

    errors: list[str] = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel="msedge", headless=True)
        context_options: dict[str, object] = {"viewport": {"width": 1440, "height": 900}}
        if args.username or args.password:
            context_options["http_credentials"] = {
                "username": args.username or "admin",
                "password": args.password or "",
            }
        context = browser.new_context(**context_options)
        page = context.new_page()
        failed_state_requests = 0

        def route_state(route) -> None:
            nonlocal failed_state_requests
            if failed_state_requests < args.fail_first_state_requests:
                failed_state_requests += 1
                route.fulfill(
                    status=503,
                    content_type="application/json",
                    body='{"ok":false,"error":"simulated startup delay"}',
                )
            else:
                route.continue_()

        if args.fail_first_state_requests:
            page.route("**/api/state", route_state)
        page.on("pageerror", lambda error: errors.append(f"page: {error}"))
        page.on(
            "console",
            lambda message: errors.append(f"console: {message.text}")
            if message.type == "error"
            and not (args.fail_first_state_requests and "503" in message.text)
            else None,
        )
        page.goto(args.url, wait_until="networkidle")
        expect(page.locator("#bootstrap-status")).to_be_hidden(timeout=15_000)
        page.locator("#page-title").wait_for(state="visible")
        expect(page.locator("#page-title")).to_have_text("运行概览")
        expect(page.locator('[data-mode="SMART"]')).to_be_visible()

        server_nav = page.locator('[data-page="servers"]')
        headless_web = not server_nav.is_visible()
        if not headless_web:
            server_nav.click()
            expect(page.locator("#page-title")).to_have_text("服务器部署")
            server_cards = page.locator(".ssh-server-card")
            if server_cards.count():
                first_card = server_cards.first
                deploy_button = first_card.locator('[data-ssh-action="deploy"]')
                assert deploy_button.is_visible()
                first_card.locator('[data-ssh-action="edit"]').click()
                expect(page.locator("#modal-ssh-region")).to_be_visible()
                expect(page.locator("#modal-ssh-region")).to_have_attribute(
                    "placeholder", "例如 美国、日本、新加坡"
                )
                page.locator("#modal-close").click()
                if first_card.locator('[data-ssh-action="copy"]:not([disabled])').count():
                    assert "已部署" in first_card.inner_text()
                    expect(deploy_button).to_contain_text("检查服务")
        else:
            expect(page.locator("#overview-ssh-row")).to_be_hidden()
            expect(page.locator("#overview-clash-endpoint")).to_be_visible()
            expect(page.locator("#overview-v2ray-endpoint")).to_be_visible()

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
        page.locator("#add-rule").click()
        expect(page.locator("#modal-rule-process-picker")).to_be_visible()
        expect(page.locator("#modal-rule-process-manual")).to_be_visible()
        expect(page.locator("#modal-rule-lines-field")).to_be_hidden()
        page.locator("#modal-rule-type").select_option("DOMAIN-SUFFIX")
        expect(page.locator("#modal-rule-process-field")).to_be_hidden()
        expect(page.locator("#modal-rule-values")).to_be_visible()
        page.locator("#modal-rule-values").fill("example.com\nexample.org")
        page.locator("#modal-close").click()
        page.evaluate(
            """() => {
                const relayNode = { name: "Smoke Relay", protocol: "ss" };
                const proxyNode = { name: "Smoke HTTP", protocol: "http" };
                appState.nodes = [
                    ...appState.nodes.filter((node) => ![relayNode.name, proxyNode.name].includes(node.name)),
                    relayNode,
                    proxyNode,
                ];
                appState.rules = [
                    ...appState.rules.filter((rule) => rule.kind !== "relay"),
                    {
                        kind: "relay",
                        enabled: true,
                        partiallyEnabled: false,
                        automatic: false,
                        ruleTypeLabel: "代理域名前置",
                        detail: "proxy.example.com:4600",
                        targetLabel: "经 Smoke Relay",
                        note: "第 2 阶段",
                        count: 1,
                        entries: [{
                            node: proxyNode.name,
                            endpoint: "proxy.example.com",
                            port: 4600,
                            relay: relayNode.name,
                            policy: "manual",
                        }],
                    },
                ];
                renderRules();
            }"""
        )
        page.locator('.rule-edit-link[data-rule-kind="relay"]').click()
        expect(page.locator("#modal-proxy-server-0")).to_have_value("proxy.example.com")
        expect(page.locator("#modal-proxy-port-0")).to_have_value("4600")
        expect(page.locator("#modal-proxy-relay-0")).to_have_value("Smoke Relay")
        page.locator("#modal-close").click()
        page.evaluate("refreshState()")
        if args.output:
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-rules.png")))

        page.locator('[data-page="nodes"]').click()
        expect(page.locator("#page-title")).to_have_text("代理与节点")
        page.locator('[data-node-tab="imported"]').click()
        expect(page.locator("#delete-error-nodes")).to_be_visible()
        expect(page.locator("#manage-node-groups")).to_be_visible()
        page.locator("#import-paste").click()
        expect(page.locator("#modal-import-name")).to_have_count(0)
        expect(page.locator("#modal-import-group")).to_be_visible()
        page.locator("#modal-actions button").first.click()
        node_groups = page.locator(".node-group-section")
        if page.locator(".node-card").count():
            first_group = node_groups.filter(has=page.locator(".node-card")).first
            group_toggle = first_group.locator("[data-node-group-toggle]")
            expect(first_group).to_be_visible()
            if group_toggle.get_attribute("aria-expanded") != "true":
                group_toggle.click()
            expect(group_toggle).to_have_attribute("aria-expanded", "true")
            expect(first_group.locator(".node-group-grid")).to_be_visible()
            group_toggle.click()
            expect(first_group.locator(".node-group-grid")).to_be_hidden()
            expect(group_toggle).to_have_attribute("aria-expanded", "false")
            page.locator("#header-refresh").click()
            expect(first_group.locator(".node-group-grid")).to_be_hidden()
            group_toggle.click()
            expect(first_group.locator(".node-group-grid")).to_be_visible()
            expect(group_toggle).to_have_attribute("aria-expanded", "true")
            page.locator(".node-card [data-node-group]").first.click()
            expect(page.locator("#modal-title")).to_have_text("移动节点到分组")
            page.locator("#modal-actions button").first.click()
        page.locator("#manage-node-groups").click()
        expect(page.locator("#modal-title")).to_have_text("节点分组管理")
        smoke_group = f"Smoke Group {int(time.time())}"
        page.locator("#new-node-group-name").fill(smoke_group)
        page.locator("#create-node-group").click()
        expect(page.locator("#app-modal")).not_to_be_visible()
        smoke_group_section = page.locator(".node-group-section").filter(
            has_text=smoke_group
        )
        expect(smoke_group_section).to_be_visible()
        expect(smoke_group_section).to_contain_text("0 个节点")

        page.locator('[data-node-tab="subscriptions"]').click()
        subscription_group = page.locator("#subscription-group")
        custom_subscription_group = page.locator(
            "#subscription-group + .custom-select"
        )
        expect(custom_subscription_group).to_be_visible()
        custom_subscription_group.locator(".custom-select-trigger").click()
        custom_subscription_group.locator(
            ".custom-select-option", has_text=smoke_group
        ).click()
        expect(subscription_group).to_have_value(smoke_group)

        page.locator('[data-node-tab="imported"]').click()
        if args.output:
            page.screenshot(path=str(args.output.with_name(args.output.stem + "-nodes.png")))
            page.set_viewport_size({"width": 820, "height": 760})
            page.screenshot(
                path=str(args.output.with_name(args.output.stem + "-nodes-narrow.png"))
            )
            page.set_viewport_size({"width": 1440, "height": 900})
        page.locator("#manage-node-groups").click()
        page.locator(f'[data-node-group-delete="{smoke_group}"]').click()
        expect(page.locator("#modal-title")).to_have_text("删除节点分组")
        page.locator("#modal-actions .danger").click()
        expect(smoke_group_section).to_have_count(0)
        page.locator('[data-page="settings"]').click()
        expect(page.locator("#page-title")).to_have_text("设置")
        if headless_web:
            expect(page.locator(".desktop-only-setting").first).to_be_hidden()
            expect(page.locator("#portable-config-import")).to_be_visible()
            expect(page.locator("#portable-config-export")).to_be_visible()

        deadline = time.monotonic() + args.poll_seconds
        while time.monotonic() < deadline:
            page.locator("#header-refresh").click()
            page.wait_for_timeout(100)
        assert "后台连接中断" not in page.locator("#overview-status-detail").inner_text()
        if args.test_close_to_tray and not headless_web:
            page.locator('[data-window-action="close"]').click()
            page.wait_for_timeout(500)
            assert page.evaluate(
                "async () => (await apiRequest('/api/state')).ok"
            ) is True

        browser.close()

    if errors:
        raise RuntimeError("\n".join(errors))
    print("WebGUI smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
