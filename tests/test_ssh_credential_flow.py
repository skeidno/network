from types import SimpleNamespace

from network_manager.models import SshServerProfile
from network_manager.ui.web_window import WebBridge


class FakeCredentialStore:
    def __init__(self) -> None:
        self.saved: list[tuple[str, str]] = []

    def set(self, profile_id: str, credential: str) -> None:
        self.saved.append((profile_id, credential))


class FakeConfigStore:
    def __init__(self) -> None:
        self.saved = 0

    def save(self, _config: object) -> None:
        self.saved += 1


class FakeWindow:
    def __init__(self, profile: SshServerProfile) -> None:
        self.profile = profile
        self.credential_store = FakeCredentialStore()
        self.store = FakeConfigStore()
        self.config = SimpleNamespace()
        self.applied = 0

    def _save_and_apply(self, _message: str) -> None:
        self.applied += 1


class FakeBridge:
    def __init__(self, profile: SshServerProfile) -> None:
        self._bridge_closed = False
        self.window = FakeWindow(profile)
        self.notifications: list[tuple[str, str]] = []

    def _ssh_profile(self, profile_id: str) -> SshServerProfile | None:
        return self.window.profile if self.window.profile.profile_id == profile_id else None

    def _ssh_target_in_use(self) -> bool:
        return False

    def _notify(self, kind: str, message: str) -> None:
        self.notifications.append((kind, message))


def test_successful_ssh_connection_persists_requested_credential() -> None:
    profile = SshServerProfile(
        profile_id="server-1",
        name="Test server",
        host="192.0.2.1",
        remember_password=False,
    )
    bridge = FakeBridge(profile)

    WebBridge._ssh_task_finished(
        bridge, "start", profile.profile_id, True, "connected", "secret"
    )

    assert bridge.window.credential_store.saved == [(profile.profile_id, "secret")]
    assert profile.remember_password is True
    assert bridge.window.store.saved == 1
    assert bridge.window.applied == 1
    assert bridge.notifications[0][0] == "success"


def test_failed_ssh_connection_does_not_persist_credential() -> None:
    profile = SshServerProfile(
        profile_id="server-1",
        name="Test server",
        host="192.0.2.1",
        remember_password=False,
    )
    bridge = FakeBridge(profile)

    WebBridge._ssh_task_finished(
        bridge, "start", profile.profile_id, False, "failed", "secret"
    )

    assert bridge.window.credential_store.saved == []
    assert profile.remember_password is False
    assert bridge.window.store.saved == 0
    assert bridge.window.applied == 0
    assert bridge.notifications == [("error", "failed")]
