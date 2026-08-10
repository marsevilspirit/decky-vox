import asyncio
import json
import os
import signal
import sys
import tempfile
import types
import unittest
from unittest import mock


class FakeLogger:
    def __init__(self):
        self.entries = []

    def _record(self, level, message, *args):
        if args:
            message = message % args
        self.entries.append((level, message))

    def info(self, message, *args):
        self._record("info", message, *args)

    def warning(self, message, *args):
        self._record("warning", message, *args)

    def error(self, message, *args):
        self._record("error", message, *args)


fake_decky = types.ModuleType("decky")
fake_decky.logger = FakeLogger()
fake_decky.emitted = []


async def fake_emit(name, *args):
    fake_decky.emitted.append((name, args))


fake_decky.emit = fake_emit
sys.modules["decky"] = fake_decky

import main  # noqa: E402  (decky must be installed in sys.modules first)


INSTANCE_ID = "core-instance-for-tests"


class FakeStdin:
    def __init__(self, process):
        self.process = process
        self.closed = False

    def write(self, data):
        if self.closed:
            raise BrokenPipeError("stdin is closed")
        for line in data.splitlines():
            message = json.loads(line.decode("utf-8"))
            self.process.requests.append(message)
            self.process.handle_request(message)

    async def drain(self):
        await asyncio.sleep(0)

    def close(self):
        if self.closed:
            return
        self.closed = True
        if self.process.exit_on_stdin_close:
            self.process.exit(0)

    async def wait_closed(self):
        await asyncio.sleep(0)


class FakeProcess:
    def __init__(self, exit_on_shutdown=True, exit_on_stdin_close=True):
        self.pid = 424242
        self.returncode = None
        self.stdout = asyncio.StreamReader()
        self.stderr = asyncio.StreamReader()
        self.stdin = FakeStdin(self)
        self.requests = []
        self.handler = None
        self.exit_on_shutdown = exit_on_shutdown
        self.exit_on_stdin_close = exit_on_stdin_close
        self._done = asyncio.get_running_loop().create_future()

    def handle_request(self, request):
        if self.handler is not None:
            self.handler(request)
            return

        method = request["method"]
        if method == "hello":
            result = {
                "protocol_version": 1,
                "instance_id": INSTANCE_ID,
                "capabilities": ["settings", "recording"],
            }
        elif method == "get_snapshot":
            result = {
                "protocol_version": 1,
                "instance_id": INSTANCE_ID,
                "seq": 0,
                "settings": {},
                "phase": "stopped",
                "enabled": False,
                "model_installed": False,
                "engine_backend": None,
                "error": None,
            }
        else:
            result = {"method": method, "params": request["params"]}
        self.respond(request["id"], result)
        if method == "shutdown" and self.exit_on_shutdown:
            asyncio.get_running_loop().call_soon(self.exit, 0)

    def send(self, message, delay=0):
        encoded = (json.dumps(message, separators=(",", ":")) + "\n").encode(
            "utf-8"
        )
        if delay:
            asyncio.get_running_loop().call_later(delay, self.stdout.feed_data, encoded)
        else:
            self.stdout.feed_data(encoded)

    def respond(self, request_id, result, delay=0):
        self.send(
            {
                "v": 1,
                "kind": "response",
                "id": request_id,
                "ok": True,
                "result": result,
            },
            delay,
        )

    def send_event(self, name, payload, seq=1, delay=0):
        self.send(
            {
                "v": 1,
                "kind": "event",
                "instance_id": INSTANCE_ID,
                "seq": seq,
                "name": name,
                "payload": payload,
            },
            delay,
        )

    def exit(self, returncode):
        if self.returncode is not None:
            return
        self.returncode = returncode
        self.stdout.feed_eof()
        self.stderr.feed_eof()
        if not self._done.done():
            self._done.set_result(returncode)

    async def wait(self):
        return await self._done


class BridgeTestCase(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        fake_decky.emitted.clear()
        fake_decky.logger.entries.clear()
        self.temporary_directory = tempfile.TemporaryDirectory()
        root = self.temporary_directory.name
        self.paths = {
            "core_path": os.path.join(root, "plugin", "bin", "decky-vox-core"),
            "plugin_dir": os.path.join(root, "plugin"),
            "settings_dir": os.path.join(root, "settings"),
            "runtime_dir": os.path.join(root, "runtime"),
            "log_dir": os.path.join(root, "logs"),
        }

    async def asyncTearDown(self):
        self.temporary_directory.cleanup()

    async def start_bridge(self, process=None, **overrides):
        process = process or FakeProcess()
        calls = []

        async def create_subprocess(*args, **kwargs):
            calls.append((args, kwargs))
            return process

        options = dict(self.paths)
        options.update(overrides)
        bridge = main.CoreBridge(**options)
        with mock.patch.object(
            main.asyncio, "create_subprocess_exec", side_effect=create_subprocess
        ):
            await bridge.start()
        return bridge, process, calls

    async def test_start_uses_fixed_argv_environment_and_handshake(self):
        with mock.patch.dict(
            os.environ,
            {"LD_LIBRARY_PATH": "/decky-temp", "LD_LIBRARY_PATH_ORIG": "/usr/lib"},
            clear=False,
        ):
            bridge, process, calls = await self.start_bridge()

        self.assertTrue(bridge.is_ready)
        self.assertEqual(
            [request["method"] for request in process.requests[:2]],
            ["hello", "get_snapshot"],
        )
        argv, options = calls[0]
        self.assertEqual(argv[0], self.paths["core_path"])
        self.assertEqual(
            argv[1:],
            (
                "--plugin-dir",
                self.paths["plugin_dir"],
                "--settings-dir",
                self.paths["settings_dir"],
                "--runtime-dir",
                self.paths["runtime_dir"],
                "--log-dir",
                self.paths["log_dir"],
            ),
        )
        self.assertTrue(options["start_new_session"])
        self.assertEqual(options["env"]["LD_LIBRARY_PATH"], "/usr/lib")
        self.assertIn(
            (
                main.BRIDGE_STATUS_EVENT_NAME,
                (
                    {
                        "bridge_instance": bridge.bridge_instance,
                        "status": "ready",
                        "code": "READY",
                    },
                ),
            ),
            fake_decky.emitted,
        )
        await bridge.close()

    async def test_start_removes_unrestorable_pyinstaller_library_path(self):
        with mock.patch.dict(
            os.environ,
            {"LD_LIBRARY_PATH": "/tmp/_MEI-decky"},
            clear=True,
        ):
            bridge, _, calls = await self.start_bridge()

        _, options = calls[0]
        self.assertNotIn("LD_LIBRARY_PATH", options["env"])
        await bridge.close()

    async def test_concurrent_response_mux_and_event_relay(self):
        process = FakeProcess()

        def handler(request):
            method = request["method"]
            if method == "hello":
                process.respond(
                    request["id"],
                    {
                        "protocol_version": 1,
                        "instance_id": INSTANCE_ID,
                        "capabilities": [],
                    },
                )
            elif method == "get_snapshot":
                process.respond(
                    request["id"],
                    {
                        "protocol_version": 1,
                        "instance_id": INSTANCE_ID,
                    },
                )
            elif method == "alpha":
                process.send_event("transcription", {"text": "你好"}, seq=7)
                process.respond(request["id"], "alpha-result", delay=0.02)
            elif method == "beta":
                process.respond(request["id"], "beta-result", delay=0.001)
            elif method == "shutdown":
                process.respond(request["id"], {})
                asyncio.get_running_loop().call_soon(process.exit, 0)

        process.handler = handler
        bridge, _, _ = await self.start_bridge(process)
        alpha = asyncio.create_task(bridge.call("alpha", {"value": 1}))
        beta = asyncio.create_task(bridge.call("beta", {"value": 2}))

        self.assertEqual(await beta, "beta-result")
        self.assertEqual(await alpha, "alpha-result")

        relayed = [
            args[0]
            for name, args in fake_decky.emitted
            if name == main.CORE_EVENT_NAME
        ]
        self.assertEqual(len(relayed), 1)
        self.assertEqual(relayed[0]["instance_id"], INSTANCE_ID)
        self.assertEqual(relayed[0]["seq"], 7)
        self.assertEqual(relayed[0]["payload"], {"text": "你好"})
        await bridge.close()

    async def test_callable_waits_for_concurrent_plugin_startup(self):
        process = FakeProcess()

        def handler(request):
            if request["method"] == "hello":
                process.respond(
                    request["id"],
                    {
                        "protocol_version": 1,
                        "instance_id": INSTANCE_ID,
                        "capabilities": [],
                    },
                )
            elif request["method"] == "get_snapshot":
                process.respond(
                    request["id"],
                    {
                        "protocol_version": 1,
                        "instance_id": INSTANCE_ID,
                    },
                    delay=0.02,
                )
            elif request["method"] == "probe":
                process.respond(request["id"], "ready-after-startup")
            elif request["method"] == "shutdown":
                process.respond(request["id"], {})
                asyncio.get_running_loop().call_soon(process.exit, 0)

        process.handler = handler

        async def create_subprocess(*_args, **_kwargs):
            return process

        bridge = main.CoreBridge(**self.paths, startup_timeout=0.5)
        with mock.patch.object(
            main.asyncio, "create_subprocess_exec", side_effect=create_subprocess
        ):
            startup = asyncio.create_task(bridge.start())
            await asyncio.sleep(0)
            probe = asyncio.create_task(bridge.call("probe", {}))
            self.assertFalse(probe.done())
            await startup

        self.assertEqual(await probe, "ready-after-startup")
        await bridge.close()

    async def test_request_timeout_fails_closed_without_restart(self):
        process = FakeProcess()

        def handler(request):
            if request["method"] in ("hello", "get_snapshot"):
                original = process.handler
                process.handler = None
                try:
                    process.handle_request(request)
                finally:
                    process.handler = original
            # "slow" is deliberately ignored.

        process.handler = handler
        bridge, _, calls = await self.start_bridge(
            process, request_timeout=0.01, terminate_timeout=0.01
        )

        with self.assertRaises(main.BridgeError) as caught:
            await bridge.call("slow", {})
        self.assertEqual(caught.exception.code, "REQUEST_TIMEOUT")
        await asyncio.sleep(0.02)
        self.assertFalse(bridge.is_ready)
        self.assertEqual(len(calls), 1)
        failure_payloads = [
            args[0]
            for name, args in fake_decky.emitted
            if name == main.BRIDGE_STATUS_EVENT_NAME
            and args[0]["status"] == "failed"
        ]
        self.assertEqual(failure_payloads[-1]["code"], "REQUEST_TIMEOUT")
        await bridge.close()

    async def test_eof_rejects_pending_request_and_does_not_restart(self):
        process = FakeProcess(exit_on_stdin_close=False)

        def handler(request):
            if request["method"] in ("hello", "get_snapshot"):
                original = process.handler
                process.handler = None
                try:
                    process.handle_request(request)
                finally:
                    process.handler = original
            # "pending" is deliberately ignored.

        process.handler = handler
        bridge, _, calls = await self.start_bridge(process, terminate_timeout=0.01)
        pending = asyncio.create_task(bridge.call("pending", {}))
        await asyncio.sleep(0)
        process.exit(17)

        with self.assertRaises(main.BridgeError) as caught:
            await pending
        self.assertEqual(caught.exception.code, "CORE_EXITED")
        self.assertEqual(len(calls), 1)
        failure_payloads = [
            args[0]
            for name, args in fake_decky.emitted
            if name == main.BRIDGE_STATUS_EVENT_NAME
            and args[0]["status"] == "failed"
        ]
        self.assertEqual(failure_payloads[-1]["code"], "CORE_EXITED")
        await bridge.close()

    async def test_protocol_mismatch_during_handshake_fails_closed(self):
        process = FakeProcess()

        def handler(request):
            if request["method"] == "hello":
                process.respond(
                    request["id"],
                    {
                        "protocol_version": 99,
                        "instance_id": INSTANCE_ID,
                        "capabilities": [],
                    },
                )

        process.handler = handler
        calls = []

        async def create_subprocess(*args, **kwargs):
            calls.append((args, kwargs))
            return process

        bridge = main.CoreBridge(**self.paths, terminate_timeout=0.01)
        with mock.patch.object(
            main.asyncio, "create_subprocess_exec", side_effect=create_subprocess
        ):
            with self.assertRaises(main.BridgeError) as caught:
                await bridge.start()
        self.assertEqual(caught.exception.code, "PROTOCOL_MISMATCH")
        await asyncio.sleep(0)
        failure_payloads = [
            args[0]
            for name, args in fake_decky.emitted
            if name == main.BRIDGE_STATUS_EVENT_NAME
            and args[0]["status"] == "failed"
        ]
        self.assertEqual(failure_payloads[-1]["code"], "PROTOCOL_MISMATCH")
        self.assertEqual(len(calls), 1)
        await bridge.close()

    async def test_unload_sends_shutdown_then_terminates_entire_group(self):
        process = FakeProcess(
            exit_on_shutdown=False,
            exit_on_stdin_close=False,
        )
        bridge, _, _ = await self.start_bridge(
            process, shutdown_timeout=0.01, terminate_timeout=0.01
        )
        signals = []

        def kill_process_group(pid, sent_signal):
            signals.append((pid, sent_signal))
            if sent_signal == signal.SIGKILL:
                process.exit(-signal.SIGKILL)

        with mock.patch.object(main.os, "killpg", side_effect=kill_process_group):
            await bridge.close()

        self.assertIn("shutdown", [request["method"] for request in process.requests])
        self.assertEqual(
            signals,
            [
                (process.pid, signal.SIGTERM),
                (process.pid, signal.SIGKILL),
            ],
        )
        self.assertTrue(bridge._stdout_task.done())
        self.assertTrue(bridge._stderr_task.done())

    async def test_plugin_callables_are_thin_exact_mappings(self):
        class StubBridge:
            def __init__(self):
                self.calls = []
                self.startup_timeout = 90.0

            async def call(self, method, params, timeout=None):
                self.calls.append((method, params, timeout))
                return {"ok": method}

        plugin = main.Plugin()
        bridge = StubBridge()
        plugin._bridge = bridge

        await plugin.hello()
        await plugin.get_snapshot()
        await plugin.update_settings({"model": "small"})
        await plugin.set_enabled(True)
        await plugin.set_enabled(False)
        await plugin.record_start(41)
        await plugin.record_stop(41)
        await plugin.cancel_session(None)
        await plugin.install_model("small")
        await plugin.cancel_model()

        self.assertEqual(
            bridge.calls,
            [
                ("hello", {"protocol_version": 1}, None),
                ("get_snapshot", {}, None),
                ("update_settings", {"settings": {"model": "small"}}, 90.0),
                ("set_enabled", {"enabled": True}, 90.0),
                ("set_enabled", {"enabled": False}, None),
                ("record_start", {"session_id": 41}, 15.0),
                ("record_stop", {"session_id": 41}, 15.0),
                ("cancel_session", {"session_id": None}, 90.0),
                ("install_model", {"model": "small"}, None),
                ("cancel_model", {}, None),
            ],
        )


if __name__ == "__main__":
    unittest.main()
