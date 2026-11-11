"""Minimal EMP daemon client for smoke tests: `from emp_rpc import connect`.

    c = connect("/tmp/emp-x")           # data dir; default ~/Library/Application Support/EMP
    c.call("threads.list", {"limit": 1})
    c.wait_for(lambda n: n["method"] == "terminal.state" and ..., timeout=10)

Notifications received while waiting for a response are kept in `c.notes`.
"""

import json
import os
import socket
import time

DEFAULT_DIR = os.path.expanduser("~/Library/Application Support/EMP")


class Client:
    def __init__(self, data_dir):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(os.path.join(data_dir, "empd.sock"))
        self.buf = b""
        self.notes = []
        self.next_id = 0
        token = open(os.path.join(data_dir, "token")).read().strip()
        self.call("hello", {"protocolVersion": 1, "token": token, "client": {"kind": "cli", "version": "0"}})

    def _read(self, timeout):
        # Raw recv + own buffer: a socket file object is unusable after a timeout.
        self.sock.settimeout(timeout)
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise EOFError("daemon closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, method, params=None, timeout=120):
        self.next_id += 1
        rid = self.next_id
        msg = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params}
        self.sock.settimeout(timeout)
        self.sock.sendall((json.dumps(msg) + "\n").encode())
        while True:
            msg = self._read(timeout)
            if msg.get("id") == rid:
                if msg.get("error"):
                    raise RuntimeError(f"{method}: {msg['error']}")
                return msg.get("result")
            if "id" not in msg:
                self.notes.append(msg)

    def wait_for(self, pred, timeout=30):
        """Returns the first notification (already received or new) matching `pred`."""
        deadline = time.time() + timeout
        for i, n in enumerate(self.notes):
            if pred(n):
                return self.notes.pop(i)
        while time.time() < deadline:
            try:
                msg = self._read(max(0.05, deadline - time.time()))
            except socket.timeout:
                break
            if "id" in msg:
                continue
            if pred(msg):
                return msg
            self.notes.append(msg)
        raise TimeoutError("notification not received")


def connect(data_dir=DEFAULT_DIR):
    return Client(data_dir)
