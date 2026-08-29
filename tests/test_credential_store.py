from __future__ import annotations

import os

import pytest

from network_manager.credential_store import CredentialStore


@pytest.mark.skipif(os.name != "nt", reason="DPAPI is only available on Windows")
def test_windows_credential_store_encrypts_password(tmp_path) -> None:
    path = tmp_path / "credentials.json"
    store = CredentialStore(path)
    store.set("server-one", "correct horse battery staple")

    assert store.get("server-one") == "correct horse battery staple"
    assert "correct horse battery staple" not in path.read_text(encoding="utf-8")

    store.delete("server-one")
    assert store.get("server-one") == ""
