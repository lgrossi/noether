from __future__ import annotations

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from unittest import TestCase, main

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "sdk/python"))

from noether_sidecar import NoetherClient, NoetherDeniedError, NoetherHttpError


class NoetherClientTests(TestCase):
    def test_client_calls_authorize_finalize_event_and_health(self) -> None:
        server = TestServer()
        server.start()
        try:
            client = NoetherClient(url=server.url, timeout=0.5, api_key="secret-token")

            decision = client.authorize(
                {
                    "project": "noether",
                    "subject": "user:local",
                    "provider": "openai",
                    "model": "gpt-4.1",
                    "metadata": {"harness": "test"},
                }
            )
            self.assertEqual(decision["outcome"], "allow")
            self.assertEqual(decision["reservation"]["id"], "reservation-1")

            reservation = client.finalize(
                "reservation-1",
                {"actual_cost_usd": 0.10, "metadata": {"trace_id": "trace-1"}},
            )
            self.assertEqual(reservation["status"], "finalized")

            self.assertEqual(
                client.event({"kind": "tool.observed", "payload": {"name": "bash"}}),
                {"accepted": True},
            )
            self.assertEqual(client.health()["status"], "ok")
            self.assertEqual(
                [(item["method"], item["path"]) for item in server.seen],
                [
                    ("POST", "/v1/authorize"),
                    ("POST", "/v1/reservations/reservation-1/finalize"),
                    ("POST", "/v1/events"),
                    ("GET", "/health"),
                ],
            )
            self.assertTrue(
                all(item["authorization"] == "Bearer secret-token" for item in server.seen)
            )
        finally:
            server.stop()

    def test_fail_open_returns_synthetic_allow(self) -> None:
        client = NoetherClient(url="http://127.0.0.1:9", timeout=0.05, fail_mode="fail_open")

        decision = client.authorize({"project": "noether"})

        self.assertEqual(decision["outcome"], "allow")
        self.assertEqual(decision["action"], "allow")
        self.assertEqual(decision["explanations"][0]["rule_id"], "sdk.sidecar_unavailable")

    def test_fail_open_does_not_synthesize_allow_for_auth_failures(self) -> None:
        server = TestServer(authorize_status=401)
        server.start()
        try:
            client = NoetherClient(
                url=server.url,
                timeout=0.5,
                fail_mode="fail_open",
                api_key="wrong-token",
            )

            with self.assertRaises(NoetherHttpError) as captured:
                client.authorize({"project": "noether"})
            self.assertEqual(captured.exception.status, 401)
        finally:
            server.stop()

    def test_fail_closed_blocks_with_decision(self) -> None:
        client = NoetherClient(url="http://127.0.0.1:9", timeout=0.05, fail_mode="fail_closed")

        decision = client.authorize({"project": "noether"})
        self.assertEqual(decision["outcome"], "deny")
        called = False

        def run(_decision: dict[str, Any], _client: NoetherClient) -> None:
            nonlocal called
            called = True

        with self.assertRaises(NoetherDeniedError):
            client.with_decision({"project": "noether"}, run)
        self.assertFalse(called)


class TestServer:
    def __init__(self, authorize_status: int = 200) -> None:
        self.seen: list[dict[str, Any]] = []
        seen = self.seen

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802
                seen.append(
                    {
                        "method": "GET",
                        "path": self.path,
                        "authorization": self.headers.get("authorization"),
                        "body": None,
                    }
                )
                if self.path == "/health":
                    self.write_json(
                        {
                            "status": "ok",
                            "decision_mode": "dry_run",
                            "policy_loaded": True,
                            "upstream_configured": False,
                            "route_count": 0,
                        }
                    )
                    return
                self.write_json({"error": "not found"}, status=404)

            def do_POST(self) -> None:  # noqa: N802
                body = self.rfile.read(int(self.headers.get("content-length", "0")))
                seen.append(
                    {
                        "method": "POST",
                        "path": self.path,
                        "authorization": self.headers.get("authorization"),
                        "body": json.loads(body.decode("utf-8")) if body else None,
                    }
                )
                if self.path == "/v1/authorize":
                    if authorize_status != 200:
                        self.write_json(
                            {"error": "missing or invalid Noether API key"},
                            status=authorize_status,
                        )
                        return
                    self.write_json(
                        {
                            "decision_id": "decision-1",
                            "outcome": "allow",
                            "action": "allow",
                            "reservation": {
                                "id": "reservation-1",
                                "amount_usd": 0.12,
                                "currency": "USD",
                                "status": "active",
                                "created_at": "2026-05-27T00:00:00Z",
                                "expires_at": "2026-05-27T01:00:00Z",
                            },
                            "explanations": [],
                            "created_at": "2026-05-27T00:00:00Z",
                        }
                    )
                    return
                if self.path == "/v1/reservations/reservation-1/finalize":
                    self.write_json(
                        {
                            "id": "reservation-1",
                            "amount_usd": 0.10,
                            "currency": "USD",
                            "status": "finalized",
                            "created_at": "2026-05-27T00:00:00Z",
                            "expires_at": "2026-05-27T01:00:00Z",
                        }
                    )
                    return
                if self.path == "/v1/events":
                    self.write_json({"accepted": True}, status=202)
                    return
                self.write_json({"error": "not found"}, status=404)

            def write_json(self, payload: dict[str, Any], status: int = 200) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format: str, *_args: Any) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        host, port = self.server.server_address
        self.url = f"http://{host}:{port}"
        self.thread = threading.Thread(target=self.server.serve_forever)

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


if __name__ == "__main__":
    main()
