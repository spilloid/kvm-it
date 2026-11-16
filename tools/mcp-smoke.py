#!/usr/bin/env python3
"""Exercise native MCP stdio and owner IPC with isolated settings and no target input."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time
import zlib


class Client:
    def __init__(self, binary: str, env: dict[str, str]):
        flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        self.process = subprocess.Popen([binary, "mcp"], env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8", creationflags=flags)
        self.messages: queue.Queue[str] = queue.Queue()
        threading.Thread(target=lambda: [self.messages.put(line) for line in self.process.stdout], daemon=True).start()
        self.seq = 0
        self.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                 "clientInfo": {"name": "kvmit-smoke", "version": "1"}})
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.process.stdin.flush()

    def call(self, method: str, params: dict):
        self.seq += 1
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params}) + "\n")
        self.process.stdin.flush()
        raw = self.messages.get(timeout=20)
        response = json.loads(raw)
        assert response["id"] == self.seq and "error" not in response, response
        return response["result"], len(raw.encode("utf-8"))

    def tool(self, name: str, arguments: dict, error: bool = False):
        response, size = self.call("tools/call", {"name": name, "arguments": arguments})
        assert response.get("isError", False) == error, response
        if error:
            return response, size
        value = response.get("structuredContent")
        assert value is not None and json.loads(response["content"][0]["text"]) == value
        return value, size

    def close(self):
        self.process.stdin.close()
        try:
            assert self.process.wait(timeout=10) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
            self.process.stdout.close()
            self.process.stderr.close()


def owner(binary: str, env: dict[str, str], access: str, endpoint: Path, approved=()):
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    endpoint.unlink(missing_ok=True)
    command = [binary, "serve", "--access", access]
    for path in approved:
        command += ["--approve", str(path)]
    process = subprocess.Popen(command, env=env,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, encoding="utf-8", creationflags=flags)
    until = time.monotonic() + 30
    while time.monotonic() < until:
        if endpoint.exists():
            try:
                metadata = json.loads(endpoint.read_text(encoding="utf-8"))
                if metadata.get("token") and metadata.get("address"):
                    return process
            except (OSError, ValueError):
                pass
        if process.poll() is not None:
            raise AssertionError(process.stderr.read())
        time.sleep(0.05)
    process.kill()
    raise AssertionError("controller endpoint did not appear")


def stop(process):
    # These test owners have deliberately nonexistent devices and never acquire hardware.
    if process.poll() is None:
        process.terminate()
    process.wait(timeout=10)
    process.stdout.close()
    process.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="kvmit-mcp-smoke-") as directory:
        root = Path(directory)
        config = root / "config.json"
        def chunk(kind, data):
            return struct.pack("!I",len(data)) + kind + data + struct.pack("!I",zlib.crc32(kind+data))
        demo = root / "synthetic.png"
        demo.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR",struct.pack("!IIBBBBB",2,1,8,6,0,0,0)) +
                         chunk(b"IDAT",zlib.compress(b"\0\xff\0\0\xff\0\0\xff\xff")) + chunk(b"IEND",b""))
        config.write_text(json.dumps({"script_dir": str(root / "scripts"), "consoles": [
            {"key": "one", "label": "Smoke", "video_key": "not-a-real-smoke-capture"}]}), encoding="utf-8")
        env = os.environ.copy()
        env["KVMIT_CONFIG_PATH"] = str(config)
        env["KVMIT_START_DISCONNECTED"] = "1"
        env["KVMIT_DEMO_VIDEO"] = str(demo)
        endpoint = config.with_suffix(".control") / "endpoint.json"
        controller = owner(binary, env, "full", endpoint)
        client = None
        try:
            client = Client(binary, env)
            tools, full_catalog_bytes = client.call("tools/list", {})
            full_tool_count = len(tools["tools"])
            assert full_tool_count >= 25
            consoles, _ = client.tool("console_list", {})
            assert consoles["consoles"][0]["console"] == "one"
            assert not consoles["consoles"][0]["connected"]
            request = {"console": "one", "library_id": "builtin:demo-notepad.toml", "request_id": "smoke-once", "dry_run": True}
            started, _ = client.tool("script_start", request)
            run_id = started["receipt"]["run_id"]
            until = time.monotonic() + 15
            while True:
                status, _ = client.tool("run_status", {"run_id": run_id})
                if status["receipt"]["status"] == "completed":
                    break
                assert time.monotonic() < until, status
                time.sleep(0.05)
            duplicate, _ = client.tool("script_start", request)
            assert duplicate["deduplicated"] and duplicate["receipt"]["run_id"] == run_id
            client.tool("script_start", {**request, "dry_run": False}, error=True)
            unchanged, unchanged_bytes = client.tool("run_status", {"run_id": run_id, "since_cursor": status["cursor"]})
            assert unchanged["changed"] is False and unchanged_bytes < 700
            videos, _ = client.tool("video_list", {})
            synthetic = next(device["path"] for device in videos["devices"] if device["path"].startswith("demo:"))
            client.tool("video_open", {"console":"one","video":synthetic})
            observed, _ = client.call("tools/call", {"name":"screen_observe","arguments":{"console":"one","image":True}})
            assert not observed.get("isError") and observed["structuredContent"]["synthetic"] is True, observed
            assert "image" not in observed["structuredContent"]
            assert any(item["type"]=="image" and item["mimeType"]=="image/png" for item in observed["content"])
            client.tool("input_perform", {"console":"one","request_id":"no-target","observation":observed["structuredContent"]["observation"],
                                          "actions":[{"kind":"key","keys":["enter"]}]}, error=True)
            client.tool("video_close", {"console":"one"})
            client.close()
            client = Client(binary, env)
            resumed, _ = client.tool("run_status", {"run_id": run_id})
            assert resumed["receipt"]["status"] == "completed" and resumed["cursor"] >= status["cursor"]
            # Test command-line front end against the same owner, not separate hardware opens.
            result = subprocess.run([binary, "control", "console_list"], env=env, capture_output=True, text=True, timeout=20)
            assert result.returncode == 0 and json.loads(result.stdout)["consoles"][0]["console"] == "one", result.stderr
        finally:
            if client is not None and client.process.poll() is None:
                client.close()
            stop(controller)
        # A new owner loads existing receipts. Old IDs never re-execute after a restart.
        controller = owner(binary, env, "observe", endpoint)
        client = None
        try:
            client = Client(binary, env)
            tools, observe_catalog_bytes = client.call("tools/list", {})
            names = {tool["name"] for tool in tools["tools"]}
            assert "input_perform" not in names and "workflow_write" not in names
            client.tool("input_perform", {"console": "one", "request_id": "denied", "observation": "unused",
                                          "actions": [{"kind": "key", "keys": ["enter"]}]}, error=True)
            persisted, _ = client.tool("run_status", {"run_id": run_id})
            assert persisted["receipt"]["status"] == "completed"
        finally:
            if client is not None and client.process.poll() is None:
                client.close()
            stop(controller)
        scripts = root / "scripts"
        scripts.mkdir(exist_ok=True)
        approvals = []
        for name in ("first", "second"):
            path = scripts / f"{name}.toml"
            path.write_text(f"workflow_format=1\nname='{name}'\nstart=['DONE']\nmatch='structure'\n"
                            "[[states]]\nname='DONE'\ndetect={text=['Ready']}\nterminal=true\n", encoding="utf-8")
            approvals.append(path)
        controller = owner(binary, env, "workflow", endpoint, approvals)
        client = None
        try:
            client = Client(binary, env)
            catalog, workflow_catalog_bytes = client.call("tools/list", {})
            names = {tool["name"] for tool in catalog["tools"]}
            assert "workflow_start" in names and "run_answer" not in names and "script_start" not in names
            library, _ = client.tool("library_list", {"console":"one"})
            tokens = {entry["library_id"]:entry["approval"] for entry in library["entries"] if entry["approval"]}
            assert "first.toml" in tokens and "second.toml" in tokens
            # The final CLI-approved workflow is selected; both secret-free approvals must remain runnable.
            for name in ("first", "second"):
                started, _ = client.tool("workflow_start", {"console":"one","library_id":f"{name}.toml",
                                                            "approval":tokens[f"{name}.toml"],"request_id":f"approved-{name}","dry_run":True})
                run = started["receipt"]["run_id"]
                client.tool("run_answer", {"run_id":run,"question_id":1,"allow":True}, error=True)
                client.tool("run_cancel", {"run_id":run})
                until = time.monotonic()+15
                while True:
                    state, _ = client.tool("run_status", {"run_id":run})
                    if state["receipt"]["status"] in ("cancelled","completed","stopped"):
                        break
                    assert time.monotonic()<until, state
                    time.sleep(0.05)
            print(json.dumps({"passed":True,"full_tool_count":full_tool_count,"workflow_tool_count":len(names),
                              "full_catalog_wire_bytes":full_catalog_bytes,"observe_catalog_wire_bytes":observe_catalog_bytes,
                              "workflow_catalog_wire_bytes":workflow_catalog_bytes,"unchanged_status_wire_bytes":unchanged_bytes,
                              "hardware_input_sent":False,"model_token_usage_measured":False}))
        finally:
            if client is not None and client.process.poll() is None:
                client.close()
            stop(controller)


if __name__ == "__main__":
    main()
