import json, os, socket

sock = socket.socket(socket.AF_UNIX)
sock.connect(os.environ["HALYARD_SOCKET"])
replies = sock.makefile("r")

def call(method, params, ids=iter(range(1, 1 << 30))):
    req = {"jsonrpc": "2.0", "id": next(ids), "method": method, "params": params}
    sock.sendall((json.dumps(req) + "\n").encode())
    msg = json.loads(replies.readline())  # no events subscribed, so this is our reply
    if "error" in msg:
        raise SystemExit(f"{method}: {msg['error']['message']} ({msg['error']['code']})")
    return msg["result"]

call("halyard.hello", {"protocols": [1], "client_name": "py-sessions"})
call("halyard.authenticate", {"token": os.environ["HALYARD_TOKEN"]})
for s in call("sessions.list", {})["sessions"]:
    print(s["id"], s["state"] or "unknown", s["title"])
