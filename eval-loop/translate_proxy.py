#!/usr/bin/env python3
# Tiny translating proxy: opencode -> here -> real local proxy.
# Rewrites `max_tokens` -> `max_completion_tokens` in chat-completion bodies,
# which the GPT-5/6 reasoning models on the local proxy require. Forwards
# everything else (incl. streaming) verbatim.
import http.server, socketserver, json, urllib.request, urllib.error

UPSTREAM = "http://localhost:6655/openai/v1"
PORT = 6656

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def _proxy(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        body = self.rfile.read(length) if length else b""
        if body:
            try:
                obj = json.loads(body)
                if isinstance(obj, dict) and "max_tokens" in obj and "max_completion_tokens" not in obj:
                    obj["max_completion_tokens"] = obj.pop("max_tokens")
                    body = json.dumps(obj).encode()
            except Exception:
                pass
        url = UPSTREAM + self.path.split("/openai/v1", 1)[-1] if "/openai/v1" in self.path else UPSTREAM + self.path
        # self.path is e.g. /v1/chat/completions or /chat/completions — normalize to upstream.
        tail = self.path
        for pre in ("/openai/v1", "/v1", "/openai"):
            if tail.startswith(pre):
                tail = tail[len(pre):]; break
        url = UPSTREAM + tail
        req = urllib.request.Request(url, data=body, method=self.command)
        for k, v in self.headers.items():
            if k.lower() in ("host", "content-length"): continue
            req.add_header(k, v)
        if body: req.add_header("Content-Length", str(len(body)))
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                data = resp.read()
                self.send_response(resp.status)
                for k, v in resp.headers.items():
                    if k.lower() in ("transfer-encoding", "content-length", "connection"): continue
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
        except urllib.error.HTTPError as e:
            data = e.read()
            self.send_response(e.code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except Exception as e:
            msg = json.dumps({"error": {"message": str(e)}}).encode()
            self.send_response(502); self.send_header("Content-Length", str(len(msg))); self.end_headers(); self.wfile.write(msg)
    def do_POST(self): self._proxy()
    def do_GET(self): self._proxy()
    def log_message(self, *a): pass

socketserver.ThreadingTCPServer.allow_reuse_address = True
with socketserver.ThreadingTCPServer(("127.0.0.1", PORT), H) as s:
    print(f"translate-proxy on :{PORT} -> {UPSTREAM}")
    s.serve_forever()
