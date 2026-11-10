# NixOS VM test: a secret used and never read (ADR-052, step 2). The daemon, the broker and curl as a machine
# runs them; a second machine is the HTTPS host (`api.example.test`, with a certificate from a test authority
# the first machine trusts), and writes down every request it is sent, so the test can see both ends: what
# reached the host, and what came back to the program that asked.
{ pkgs, self }:

let
  # The test's own passwords, as tests/socket.nix has them: dev is the owner (wheel), sam is not.
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
  samHash = "$6$torqsocketsal2$p4ihhGlRahyPpWY43xyA52iNOzVtnXF1dQCVxf0eBeRehXODXyWOdptk3znfwCfmhj5ERP28izhmIRqrAL5J00";
  certs = pkgs.runCommand "torq-secrets-test-certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=TorqOS test authority" -keyout $out/ca.key -out $out/ca.crt
    openssl req -newkey rsa:2048 -nodes -subj "/CN=api.example.test" -keyout $out/server.key -out server.csr
    printf 'subjectAltName=DNS:api.example.test,DNS:other.example.test\n' > ext
    openssl x509 -req -in server.csr -CA $out/ca.crt -CAkey $out/ca.key -CAcreateserial -days 3650 -extfile ext -out $out/server.crt
  '';
  # The host: what it is sent, written down; what it answers, by path.
  server = pkgs.writeText "host.py" ''
    import http.server, json, ssl, gzip
    class H(http.server.BaseHTTPRequestHandler):
        def answer(self):
            n = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(n).decode() if n else ""
            seen = {"method": self.command, "path": self.path, "headers": dict(self.headers.items()), "body": body}
            with open("/tmp/requests.log", "a") as f:
                f.write(json.dumps(seen) + "\n")
            if self.path.startswith("/redirect"):
                self.send_response(302)
                self.send_header("Location", "https://other.example.test/stolen")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            out = json.dumps(seen).encode()
            if self.path.startswith("/gzip"):
                out = gzip.compress(out)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            if self.path.startswith("/gzip"):
                self.send_header("Content-Encoding", "gzip")
            # An echo in a header too, as a debugging server may.
            self.send_header("X-Echo", self.headers.get("Authorization") or "")
            # As GitHub's API does: any web page may read the answer.
            if self.path.startswith("/cors"):
                self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Content-Length", str(len(out)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(out)
        do_GET = do_POST = do_PUT = do_DELETE = do_HEAD = answer
    s = http.server.ThreadingHTTPServer(("0.0.0.0", 443), H)
    c = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    c.load_cert_chain("${certs}/server.crt", "${certs}/server.key")
    s.socket = c.wrap_socket(s.socket, server_side=True)
    s.serve_forever()
  '';
in
pkgs.testers.runNixOSTest {
  name = "torq-secrets";

  nodes.far = { pkgs, ... }: {
    networking.firewall.allowedTCPPorts = [ 443 ];
    systemd.services.host = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${server}";
    };
  };

  nodes.machine = { pkgs, lib, nodes, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = samHash; };
    torq.broker.rebuildCommand = "true";
    security.pki.certificateFiles = [ "${certs}/ca.crt" ];
    networking.hosts.${nodes.far.networking.primaryIPAddress} = [ "api.example.test" "other.example.test" ];
    environment.systemPackages = [ pkgs.socat pkgs.curl pkgs.jq ];
  };

  testScript = ''
    import json, shlex
    start_all()
    far.wait_for_unit("host.service")
    far.wait_for_open_port(443)
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{ }}' > {repo}/desktop.nix && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")
    machine.wait_for_unit("torq-secrets.service")
    machine.wait_for_open_port(7800)

    def ask(user, request):
        line = json.dumps(request)
        cmd = f"printf '%s\\n' {shlex.quote(line)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        raw = machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}")
        code, out = None, ""
        for l in raw.splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            if "exit" in f:
                code = f["exit"]
        return code, json.loads(out) if out.strip() else None

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def requests():
        return [json.loads(l) for l in far.succeed("cat /tmp/requests.log 2>/dev/null || true").splitlines()]

    def use(user, path, *flags):
        """A request through the daemon, as `user`: (status, body, headers)."""
        args = " ".join(shlex.quote(f) for f in flags)
        cmd = f"curl -s -D /tmp/h-{user} -o /tmp/b-{user} -w '%{{http_code}}' {args} http://127.0.0.1:7800{path}"
        status = int(machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}").strip())
        body = machine.succeed(f"cat /tmp/b-{user}")
        heads = machine.succeed(f"cat /tmp/h-{user}")
        return status, body, heads

    with subtest("dev keeps a token, and says the agent may use it for api.example.test"):
        code, v = ask("dev", {"args": ["--json", "secret-set", "--name", "gh", "--user", "dev"], "key": "dev-token-1"})
        assert code == 0, (code, v)
        rules = '{"gh": {"kind": "token", "https": {"hosts": ["api.example.test"]}, "actors": ["torq-agent"], "approve": "never"}}'
        diff = f"--- /dev/null\n+++ b/users/dev/secrets.json\n@@ -0,0 +1 @@\n+{rules}\n"
        code, p = ask("dev", {"args": ["--json", "propose", f"--base={head()}", "--reason=gh for the agent"], "diff": diff})
        assert code == 0, (code, p)
        code, v = ask("dev", {"args": ["--json", "apply", str(p["seq"])]})
        assert code == 0, (code, v)

    with subtest("its owner uses it: the host gets the value, the owner's program never sees it"):
        status, body, heads = use("dev", "/user/dev/gh/api.example.test/echo?x=1")
        assert status == 200, (status, body)
        sent = requests()[-1]
        assert sent["headers"]["Authorization"] == "Bearer dev-token-1" and sent["path"] == "/echo?x=1", sent
        assert "dev-token-1" not in body and "Bearer [secret]" in body, body
        assert "dev-token-1" not in heads and "[secret]" in heads, heads
        assert sent["headers"]["Host"] == "api.example.test", sent

    with subtest("the agent uses it from its session, never reading it; its first use is on the record once"):
        code, s = ask("torq-agent", {"args": ["--json", "session-start"]})
        assert code == 0, (code, s)
        session = s["session"]
        for _ in range(2):
            status, body, _ = use("torq-agent", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}")
            assert status == 200 and "dev-token-1" not in body, (status, body)
        assert "x-torq-session" not in {k.lower() for k in requests()[-1]["headers"]}, "the session's name stays here"
        ledger = machine.succeed("torq-broker log")
        assert ledger.count("secret_first_use") == 1, ledger
        assert "dev-token-1" not in ledger
        # Its own Authorization is not the one sent: the secret's takes its place.
        status, _, _ = use("torq-agent", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}", "-H", "Authorization: Bearer the-agents-own")
        assert status == 200 and requests()[-1]["headers"]["Authorization"] == "Bearer dev-token-1", requests()[-1]
        # A POST's body goes as it was sent.
        status, body, _ = use("torq-agent", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}", "-X", "POST", "--data-binary", '{"a": 1}')
        assert status == 200 and requests()[-1]["body"] == '{"a": 1}' and requests()[-1]["method"] == "POST", requests()[-1]

    with subtest("nowhere else, by no one else, and not without a session"):
        n = len(requests())
        for user, path, flags in [
            ("torq-agent", "/user/dev/gh/other.example.test/echo", ["-H", f"X-Torq-Session: {session}"]),
            ("torq-agent", "/user/dev/gh/api.example.test/echo", []),
            ("sam", "/user/dev/gh/api.example.test/echo", []),
            ("dev", "/system/gh/api.example.test/echo", []),
            ("torq-agent", "/user/dev/nothing/api.example.test/echo", ["-H", f"X-Torq-Session: {session}"]),
        ]:
            status, body, _ = use(user, path, *flags)
            assert status == 403 and "dev-token-1" not in body, (user, path, status, body)
        assert len(requests()) == n, "a refused use sends nothing"
        # Bad requests are refused before anything is asked.
        status, _, _ = use("torq-agent", "/user/dev/gh/10.0.0.1/echo")
        assert status == 400, status

    with subtest("a web page the owner has open sends as the owner: what a browser sends is refused"):
        n = len(requests())
        for flags in [["-H", "Origin: https://evil.example"], ["-H", "Referer: https://evil.example/"], ["-H", "Sec-Fetch-Site: cross-site"],
                      ["-H", "Host: evil.example:7800"], ["-X", "OPTIONS"]]:
            status, body, _ = use("dev", "/user/dev/gh/api.example.test/echo", *flags)
            assert status in (400, 403) and "dev-token-1" not in body, (flags, status, body)
        assert len(requests()) == n, "nothing sent"
        # What would let a page read an answer (CORS) is not passed on.
        status, _, heads = use("dev", "/user/dev/gh/api.example.test/cors")
        assert status == 200 and "access-control" not in heads.lower(), heads

    with subtest("a redirect is not followed: the value goes to the rule's host and no other"):
        n = len(requests())
        status, _, heads = use("dev", "/user/dev/gh/api.example.test/redirect")
        assert status == 302 and "other.example.test" in heads, (status, heads)
        assert len(requests()) == n + 1 and requests()[-1]["path"] == "/redirect", requests()[-1]

    with subtest("an answer that cannot be checked for the value is not passed on"):
        status, body, _ = use("dev", "/user/dev/gh/api.example.test/gzip")
        assert status == 502 and "compressed" in body, (status, body)

    with subtest("a tainted session asks a person, and asking is not built yet: refused"):
        code, _ = ask("torq-agent", {"args": ["--json", "session-taint", session, "--source", "web"]})
        assert code == 0
        status, body, _ = use("torq-agent", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}")
        assert status == 403 and "untrusted" in body, (status, body)

    with subtest("no one but the daemon asks the broker for a use, and the value is nowhere it can be read"):
        for user in ["torq-agent", "dev", "sam"]:
            code, v = ask(user, {"args": ["--json", "secret-use", "--name", "gh", "--user", "dev", "--host", "api.example.test", "--client-port", "1", "--server-port", "7800"]})
            assert code == 1, (user, code, v)
        pid = machine.succeed("systemctl show -p MainPID --value torq-secrets").strip()
        machine.fail(f"su -s /bin/sh torq-agent -c 'cat /proc/{pid}/environ'")
        machine.fail("su -s /bin/sh torq-agent -c 'ls /var/lib/torq/secrets/store'")
        journal = machine.succeed("journalctl -u torq-secrets --no-pager")
        assert "dev-token-1" not in journal, "never in the daemon's log"
        assert "torq-agent used dev's secret gh for GET https://api.example.test/echo: 200" in journal, journal
        assert "dev used dev's secret gh" in journal
  '';
}
