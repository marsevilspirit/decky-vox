"""Decky Loader bridge for the Decky Vox Rust core.

This module deliberately contains no voice-typing business logic.  It owns the
Rust child process, multiplexes versioned NDJSON requests, and relays events to
the Decky frontend.
"""

import asyncio
import itertools
import json
import os
import signal
import uuid
from typing import Any, Dict, Optional

import decky


PROTOCOL_VERSION = 1
MAX_LINE_BYTES = 1024 * 1024
DEFAULT_REQUEST_TIMEOUT_SECONDS = 10.0
DEFAULT_CONTROL_TIMEOUT_SECONDS = 15.0
# Vulkan readiness may consume 30 seconds before a bounded CPU fallback gets
# its own readiness window. Normal RPCs remain capped at 10 seconds.
DEFAULT_STARTUP_TIMEOUT_SECONDS = 90.0
DEFAULT_SHUTDOWN_TIMEOUT_SECONDS = 2.5
DEFAULT_TERMINATE_TIMEOUT_SECONDS = 0.5

CORE_EVENT_NAME = "decky_vox_event"
BRIDGE_STATUS_EVENT_NAME = "decky_vox_bridge_status"


class BridgeError(RuntimeError):
    """Base error exposed when an IPC request cannot complete safely."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__("{}: {}".format(code, message))
        self.code = code
        self.message = message


class RemoteCoreError(BridgeError):
    """A structured error response returned by the Rust core."""

    def __init__(self, error: Any) -> None:
        if isinstance(error, dict):
            code = error.get("code")
            message = error.get("message")
            if isinstance(code, str) and isinstance(message, str):
                self.error = error
                super().__init__(code, message)
                return
        self.error = error
        super().__init__("CORE_ERROR", "Rust core returned an invalid error payload")


class CoreBridge:
    """Supervise one Rust core instance and multiplex its stdio protocol."""

    def __init__(
        self,
        core_path: str,
        plugin_dir: str,
        settings_dir: str,
        runtime_dir: str,
        log_dir: str,
        request_timeout: float = DEFAULT_REQUEST_TIMEOUT_SECONDS,
        startup_timeout: float = DEFAULT_STARTUP_TIMEOUT_SECONDS,
        shutdown_timeout: float = DEFAULT_SHUTDOWN_TIMEOUT_SECONDS,
        terminate_timeout: float = DEFAULT_TERMINATE_TIMEOUT_SECONDS,
    ) -> None:
        self.core_path = os.path.abspath(core_path)
        self.plugin_dir = os.path.abspath(plugin_dir)
        self.settings_dir = os.path.abspath(settings_dir)
        self.runtime_dir = os.path.abspath(runtime_dir)
        self.log_dir = os.path.abspath(log_dir)
        self.request_timeout = request_timeout
        self.startup_timeout = startup_timeout
        self.shutdown_timeout = shutdown_timeout
        self.terminate_timeout = terminate_timeout

        self.bridge_instance = uuid.uuid4().hex
        self.process = None  # type: Optional[asyncio.subprocess.Process]
        self._writer_lock = asyncio.Lock()
        self._lifecycle_lock = asyncio.Lock()
        self._request_ids = itertools.count(1)
        self._pending = {}  # type: Dict[int, asyncio.Future]
        self._stdout_task = None  # type: Optional[asyncio.Task]
        self._stderr_task = None  # type: Optional[asyncio.Task]
        self._abort_task = None  # type: Optional[asyncio.Task]
        self._startup_done = asyncio.Event()
        self._ready = False
        self._closing = False
        self._failure = None  # type: Optional[BridgeError]
        self._failure_reported = False

    @property
    def is_ready(self) -> bool:
        return self._ready and self._failure is None and not self._closing

    def _argv(self):
        return [
            self.core_path,
            "--plugin-dir",
            self.plugin_dir,
            "--settings-dir",
            self.settings_dir,
            "--runtime-dir",
            self.runtime_dir,
            "--log-dir",
            self.log_dir,
        ]

    def _child_environment(self) -> Dict[str, str]:
        env = os.environ.copy()

        # Decky is packaged with PyInstaller.  Its temporary library path must
        # not leak into independently-built Rust/voxtype executables.
        original_library_path = env.get("LD_LIBRARY_PATH_ORIG")
        if original_library_path is not None:
            if original_library_path:
                env["LD_LIBRARY_PATH"] = original_library_path
            else:
                env.pop("LD_LIBRARY_PATH", None)
        else:
            # PyInstaller may inject its temporary _MEI directory without an
            # *_ORIG marker when the host originally had no library path.
            env.pop("LD_LIBRARY_PATH", None)

        # Preserve Decky's valid value.  If it is absent, SteamOS normally
        # exposes PipeWire under /run/user/<uid>.
        if not env.get("XDG_RUNTIME_DIR"):
            steam_runtime_dir = "/run/user/{}".format(os.getuid())
            if os.path.isdir(steam_runtime_dir):
                env["XDG_RUNTIME_DIR"] = steam_runtime_dir

        return env

    async def start(self) -> None:
        async with self._lifecycle_lock:
            if self.is_ready:
                return
            if self.process is not None:
                raise BridgeError("BACKEND_UNAVAILABLE", "Rust core is already starting")

            for directory in (self.settings_dir, self.runtime_dir, self.log_dir):
                os.makedirs(directory, exist_ok=True)

            try:
                self.process = await asyncio.create_subprocess_exec(
                    *self._argv(),
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    env=self._child_environment(),
                    start_new_session=True,
                    limit=MAX_LINE_BYTES + 1,
                )
            except Exception as error:
                bridge_error = BridgeError(
                    "BACKEND_UNAVAILABLE",
                    "Unable to start Rust core: {}".format(error),
                )
                await self._mark_failed(bridge_error)
                raise bridge_error

            self._stdout_task = asyncio.create_task(self._read_stdout())
            self._stderr_task = asyncio.create_task(self._read_stderr())

            try:
                hello = await self._request(
                    "hello",
                    {"protocol_version": PROTOCOL_VERSION},
                    timeout=self.startup_timeout,
                    allow_starting=True,
                )
                instance_id = self._validate_hello(hello)

                snapshot = await self._request(
                    "get_snapshot",
                    {},
                    timeout=self.startup_timeout,
                    allow_starting=True,
                )
                self._validate_snapshot(snapshot, instance_id)
            except BridgeError as error:
                if self._failure is None:
                    await self._mark_failed(error)
                self._schedule_abort()
                raise
            except Exception as error:
                bridge_error = BridgeError(
                    "PROTOCOL_MISMATCH",
                    "Invalid Rust core handshake: {}".format(error),
                )
                await self._mark_failed(bridge_error)
                self._schedule_abort()
                raise bridge_error

            self._ready = True
            self._startup_done.set()
            await self._emit_bridge_status("ready", "READY")

    def _validate_hello(self, result: Any) -> str:
        if not isinstance(result, dict):
            raise BridgeError("PROTOCOL_MISMATCH", "hello result must be an object")
        if result.get("protocol_version") != PROTOCOL_VERSION:
            raise BridgeError("PROTOCOL_MISMATCH", "Unsupported core protocol version")
        instance_id = result.get("instance_id")
        if not isinstance(instance_id, str) or not instance_id:
            raise BridgeError("PROTOCOL_MISMATCH", "hello result has no instance_id")
        capabilities = result.get("capabilities")
        if not isinstance(capabilities, list):
            raise BridgeError("PROTOCOL_MISMATCH", "hello result has no capabilities")
        return instance_id

    def _validate_snapshot(self, result: Any, instance_id: str) -> None:
        if not isinstance(result, dict):
            raise BridgeError("PROTOCOL_MISMATCH", "snapshot must be an object")
        if result.get("protocol_version") != PROTOCOL_VERSION:
            raise BridgeError("PROTOCOL_MISMATCH", "snapshot protocol version differs")
        if result.get("instance_id") != instance_id:
            raise BridgeError("PROTOCOL_MISMATCH", "snapshot core instance differs")

    async def call(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        timeout: Optional[float] = None,
    ) -> Any:
        if not self.is_ready and self._failure is None and not self._closing:
            # Decky starts Plugin._main and the callable socket concurrently.
            # A frontend call may therefore arrive while the bounded Rust
            # handshake/auto-start sequence is still running. Share the same
            # readiness gate instead of turning that benign race into a
            # permanent BACKEND_UNAVAILABLE state in the frontend.
            try:
                await asyncio.wait_for(
                    self._startup_done.wait(), timeout=(self.startup_timeout * 2) + 5.0
                )
            except asyncio.TimeoutError:
                raise BridgeError(
                    "BACKEND_UNAVAILABLE", "Rust core startup is still incomplete"
                )
        if not self.is_ready:
            if self._failure is not None:
                raise self._failure
            raise BridgeError("BACKEND_UNAVAILABLE", "Rust core is not ready")
        return await self._request(method, params or {}, timeout=timeout)

    async def _request(
        self,
        method: str,
        params: Dict[str, Any],
        timeout: Optional[float] = None,
        allow_starting: bool = False,
        allow_closing: bool = False,
    ) -> Any:
        if self._closing and not allow_closing:
            raise BridgeError("BRIDGE_CLOSED", "Decky Vox bridge is unloading")
        if self._failure is not None:
            raise self._failure
        if self.process is None or self.process.stdin is None:
            raise BridgeError("BACKEND_UNAVAILABLE", "Rust core is not running")
        if not allow_starting and not self._ready:
            raise BridgeError("BACKEND_UNAVAILABLE", "Rust core is not ready")

        request_id = next(self._request_ids)
        loop = asyncio.get_running_loop()
        future = loop.create_future()
        self._pending[request_id] = future
        message = {
            "v": PROTOCOL_VERSION,
            "kind": "request",
            "id": request_id,
            "method": method,
            "params": params,
        }

        try:
            encoded = (json.dumps(message, separators=(",", ":")) + "\n").encode(
                "utf-8"
            )
            if len(encoded) > MAX_LINE_BYTES:
                raise BridgeError("REQUEST_TOO_LARGE", "IPC request exceeds size limit")

            async with self._writer_lock:
                self.process.stdin.write(encoded)
                await self.process.stdin.drain()
        except BridgeError:
            self._pending.pop(request_id, None)
            if not future.done():
                future.cancel()
            raise
        except Exception as error:
            self._pending.pop(request_id, None)
            if not future.done():
                future.cancel()
            bridge_error = BridgeError(
                "BACKEND_UNAVAILABLE", "Unable to write to Rust core: {}".format(error)
            )
            await self._mark_failed(bridge_error)
            self._schedule_abort()
            raise bridge_error

        request_timeout = self.request_timeout if timeout is None else timeout
        try:
            return await asyncio.wait_for(
                asyncio.shield(future), timeout=request_timeout
            )
        except asyncio.CancelledError:
            self._pending.pop(request_id, None)
            if not future.done():
                future.cancel()
            raise
        except asyncio.TimeoutError:
            self._pending.pop(request_id, None)
            if not future.done():
                future.cancel()
            bridge_error = BridgeError(
                "REQUEST_TIMEOUT", "Rust core did not answer {} in time".format(method)
            )
            await self._mark_failed(bridge_error)
            self._schedule_abort()
            raise bridge_error

    async def _read_stdout(self) -> None:
        process = self.process
        if process is None or process.stdout is None:
            return

        try:
            while True:
                try:
                    raw_line = await process.stdout.readline()
                except (ValueError, asyncio.LimitOverrunError):
                    raise BridgeError(
                        "PROTOCOL_MISMATCH", "Rust core emitted an oversized line"
                    )

                if not raw_line:
                    if not self._closing:
                        exit_code = process.returncode
                        detail = "unknown" if exit_code is None else str(exit_code)
                        await self._mark_failed(
                            BridgeError(
                                "CORE_EXITED",
                                "Rust core stdout closed (exit code {})".format(detail),
                            )
                        )
                        self._schedule_abort()
                    return

                if len(raw_line) > MAX_LINE_BYTES or not raw_line.endswith(b"\n"):
                    raise BridgeError(
                        "PROTOCOL_MISMATCH", "Rust core emitted an invalid NDJSON line"
                    )

                try:
                    message = json.loads(raw_line.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise BridgeError(
                        "PROTOCOL_MISMATCH",
                        "Rust core emitted malformed JSON: {}".format(error),
                    )

                await self._dispatch_message(message)
        except asyncio.CancelledError:
            raise
        except BridgeError as error:
            if not self._closing:
                await self._mark_failed(error)
                self._schedule_abort()
        except Exception as error:
            if not self._closing:
                await self._mark_failed(
                    BridgeError(
                        "PROTOCOL_MISMATCH",
                        "Rust core reader failed: {}".format(error),
                    )
                )
                self._schedule_abort()

    async def _dispatch_message(self, message: Any) -> None:
        if not isinstance(message, dict) or message.get("v") != PROTOCOL_VERSION:
            raise BridgeError("PROTOCOL_MISMATCH", "Invalid protocol envelope")

        kind = message.get("kind")
        if kind == "response":
            request_id = message.get("id")
            if (
                not isinstance(request_id, int)
                or isinstance(request_id, bool)
                or not isinstance(message.get("ok"), bool)
            ):
                raise BridgeError("PROTOCOL_MISMATCH", "Invalid response envelope")

            if message["ok"] and "result" not in message:
                raise BridgeError(
                    "PROTOCOL_MISMATCH", "Successful response has no result"
                )
            if not message["ok"] and "error" not in message:
                raise BridgeError("PROTOCOL_MISMATCH", "Failed response has no error")

            future = self._pending.pop(request_id, None)
            if future is None or future.done():
                # A response can legitimately arrive after its request timed out.
                return

            if message["ok"]:
                future.set_result(message["result"])
            else:
                future.set_exception(RemoteCoreError(message["error"]))
            return

        if kind == "event":
            instance_id = message.get("instance_id")
            sequence = message.get("seq")
            name = message.get("name")
            if (
                not isinstance(instance_id, str)
                or not instance_id
                or not isinstance(sequence, int)
                or isinstance(sequence, bool)
                or sequence < 0
                or not isinstance(name, str)
                or not name
                or "payload" not in message
            ):
                raise BridgeError("PROTOCOL_MISMATCH", "Invalid event envelope")
            await decky.emit(CORE_EVENT_NAME, message)
            return

        raise BridgeError("PROTOCOL_MISMATCH", "Unexpected Rust core message kind")

    async def _read_stderr(self) -> None:
        process = self.process
        if process is None or process.stderr is None:
            return
        try:
            while True:
                line = await process.stderr.readline()
                if not line:
                    return
                # Never mirror stderr to stdout: stdout is the core protocol.
                decky.logger.info(
                    "decky-vox-core: %s",
                    line.decode("utf-8", errors="replace").rstrip(),
                )
        except asyncio.CancelledError:
            raise
        except Exception as error:
            decky.logger.warning("decky-vox-core stderr reader failed: %s", error)

    async def _mark_failed(self, error: BridgeError) -> None:
        if self._failure is None:
            self._failure = error
        self._ready = False
        self._startup_done.set()

        for future in list(self._pending.values()):
            if not future.done():
                future.set_exception(self._failure)
        self._pending.clear()

        if not self._failure_reported and not self._closing:
            self._failure_reported = True
            await self._emit_bridge_status(
                "failed", self._failure.code, self._failure.message
            )

    async def _emit_bridge_status(
        self, status: str, code: str, message: Optional[str] = None
    ) -> None:
        payload = {
            "bridge_instance": self.bridge_instance,
            "status": status,
            "code": code,
        }
        if message:
            payload["message"] = message
        try:
            await decky.emit(BRIDGE_STATUS_EVENT_NAME, payload)
        except Exception as error:
            decky.logger.warning("Unable to emit Decky Vox bridge status: %s", error)

    def _schedule_abort(self) -> None:
        if self._closing:
            return
        if self._abort_task is None or self._abort_task.done():
            self._abort_task = asyncio.create_task(self._terminate_process_group())

    async def _wait_for_process(self, timeout: float) -> bool:
        process = self.process
        if process is None:
            return True
        if process.returncode is not None:
            return True
        try:
            await asyncio.wait_for(asyncio.shield(process.wait()), timeout=timeout)
            return True
        except asyncio.TimeoutError:
            return False

    def _signal_process_group(self, sig: signal.Signals) -> None:
        process = self.process
        if process is None or process.returncode is not None:
            return
        pid = process.pid
        if pid is None or pid <= 1 or pid == os.getpgrp():
            decky.logger.error("Refusing to signal unsafe Rust core pid: %s", pid)
            return
        try:
            # start_new_session=True makes the child PID its process-group ID.
            os.killpg(pid, sig)
        except ProcessLookupError:
            return
        except PermissionError as error:
            decky.logger.error("Unable to signal Rust core process group: %s", error)

    async def _terminate_process_group(self) -> None:
        process = self.process
        if process is None:
            return

        if process.stdin is not None:
            try:
                process.stdin.close()
            except Exception:
                pass

        if await self._wait_for_process(0):
            return
        self._signal_process_group(signal.SIGTERM)
        if await self._wait_for_process(self.terminate_timeout):
            return
        self._signal_process_group(signal.SIGKILL)
        await self._wait_for_process(self.terminate_timeout)

    async def close(self) -> None:
        async with self._lifecycle_lock:
            if self._closing:
                return
            self._closing = True
            self._ready = False
            self._startup_done.set()
            process = self.process

            if process is not None and process.returncode is None and self._failure is None:
                shutdown_task = asyncio.create_task(
                    self._request(
                        "shutdown", {}, allow_starting=True, allow_closing=True
                    )
                )
                try:
                    await self._wait_for_process(self.shutdown_timeout)
                finally:
                    if not shutdown_task.done():
                        shutdown_task.cancel()
                    await asyncio.gather(shutdown_task, return_exceptions=True)

            if process is not None and process.returncode is None:
                await self._terminate_process_group()

            closing_error = BridgeError("BRIDGE_CLOSED", "Decky Vox bridge unloaded")
            for future in list(self._pending.values()):
                if not future.done():
                    future.set_exception(closing_error)
            self._pending.clear()

            current = asyncio.current_task()
            tasks = [self._stdout_task, self._stderr_task, self._abort_task]
            active_tasks = [
                task
                for task in tasks
                if task is not None and task is not current and not task.done()
            ]
            for task in active_tasks:
                task.cancel()
            if active_tasks:
                await asyncio.gather(*active_tasks, return_exceptions=True)

            if process is not None and process.stdin is not None:
                try:
                    process.stdin.close()
                    wait_closed = getattr(process.stdin, "wait_closed", None)
                    if wait_closed is not None:
                        await wait_closed()
                except Exception:
                    pass


def _decky_directory(primary: str, fallback: str, default: str) -> str:
    value = getattr(decky, primary, None) or getattr(decky, fallback, None) or default
    return os.path.abspath(value)


class Plugin:
    """Decky lifecycle and callable surface; every callable forwards to Rust."""

    async def _main(self) -> None:
        plugin_dir = _decky_directory(
            "DECKY_PLUGIN_DIR", "DECKY_CURRENT_PLUGIN_DIR", os.path.dirname(__file__)
        )
        settings_dir = _decky_directory(
            "DECKY_PLUGIN_SETTINGS_DIR",
            "DECKY_SETTINGS_DIR",
            os.path.join(plugin_dir, "data", "settings"),
        )
        runtime_dir = _decky_directory(
            "DECKY_PLUGIN_RUNTIME_DIR",
            "DECKY_RUNTIME_DIR",
            os.path.join(plugin_dir, "data", "runtime"),
        )
        log_dir = _decky_directory(
            "DECKY_PLUGIN_LOG_DIR",
            "DECKY_LOG_DIR",
            os.path.join(plugin_dir, "data", "logs"),
        )

        self._bridge = CoreBridge(
            core_path=os.path.join(plugin_dir, "bin", "decky-vox-core"),
            plugin_dir=plugin_dir,
            settings_dir=settings_dir,
            runtime_dir=runtime_dir,
            log_dir=log_dir,
        )
        try:
            await self._bridge.start()
        except BridgeError as error:
            # Keep the Decky frontend loadable so it can display the bridge
            # status.  v1 intentionally performs no automatic restart.
            decky.logger.error("Decky Vox backend unavailable: %s", error)

    def _core(self) -> CoreBridge:
        bridge = getattr(self, "_bridge", None)
        if bridge is None:
            raise BridgeError("BACKEND_UNAVAILABLE", "Decky Vox bridge has not started")
        return bridge

    async def hello(self) -> Any:
        return await self._core().call(
            "hello", {"protocol_version": PROTOCOL_VERSION}
        )

    async def get_snapshot(self) -> Any:
        return await self._core().call("get_snapshot", {})

    async def update_settings(self, settings: Dict[str, Any]) -> Any:
        bridge = self._core()
        return await bridge.call(
            "update_settings",
            {"settings": settings},
            timeout=bridge.startup_timeout,
        )

    async def set_enabled(self, enabled: bool) -> Any:
        bridge = self._core()
        return await bridge.call(
            "set_enabled",
            {"enabled": enabled},
            timeout=bridge.startup_timeout if enabled else None,
        )

    async def record_start(self, session_id: int) -> Any:
        bridge = self._core()
        return await bridge.call(
            "record_start",
            {"session_id": session_id},
            # Each session cold-starts the model, including bounded GPU fallback.
            timeout=bridge.startup_timeout,
        )

    async def record_stop(self, session_id: int) -> Any:
        return await self._core().call(
            "record_stop",
            {"session_id": session_id},
            timeout=DEFAULT_CONTROL_TIMEOUT_SECONDS,
        )

    async def cancel_session(self, session_id: Optional[int] = None) -> Any:
        bridge = self._core()
        return await bridge.call(
            "cancel_session",
            {"session_id": session_id},
            timeout=bridge.startup_timeout,
        )

    async def install_model(self, model: str) -> Any:
        return await self._core().call("install_model", {"model": model})

    async def cancel_model(self) -> Any:
        return await self._core().call("cancel_model", {})

    async def _unload(self) -> None:
        bridge = getattr(self, "_bridge", None)
        if bridge is not None:
            await bridge.close()

    async def _uninstall(self) -> None:
        # Models/settings are intentionally not deleted by this thin bridge.
        return None

    async def _migration(self) -> None:
        # Rust owns settings schema migration; Python must not rewrite it.
        return None
