# NixOS VM test: a secret used and never read (ADR-052, step 2). The daemon, the broker and curl as a machine
# runs them; a second machine is the HTTPS host (`api.example.test`, with a certificate from a test authority
# the first machine trusts), and writes down every request it is sent, so the test can see both ends: what
# reached the host, and what came back to the program that asked.
# Last, the way TorqOS Settings keeps a value (step 5): usable, and in no file of the machine.
{ pkgs, self }:

let
  # The test's own passwords, as tests/socket.nix has them: dev is the owner (wheel), sam is not.
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
  samHash = "$6$torqsocketsal2$p4ihhGlRahyPpWY43xyA52iNOzVtnXF1dQCVxf0eBeRehXODXyWOdptk3znfwCfmhj5ERP28izhmIRqrAL5J00";
  # TorqOS Settings' way of keeping a value, without the window (tests/secret-set-example.nix).
  windowSet = import ./secret-set-example.nix { inherit pkgs self; };
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
    # A git host over SSH: `git`, whose key is put in by the test.
    services.openssh = { enable = true; settings.PasswordAuthentication = false; };
    users.users.git = { isNormalUser = true; };
    # Light: sshd and a small server, on a laptop that runs the test.
    virtualisation.memorySize = 512;
  };

  # Another SSH host the key is let in at, which no rule names: the key must not sign for it.
  nodes.elsewhere = { ... }: {
    services.openssh = { enable = true; settings.PasswordAuthentication = false; };
    users.users.git = { isNormalUser = true; };
    # Light: sshd and a small server, on a laptop that runs the test.
    virtualisation.memorySize = 512;
  };

  nodes.machine = { pkgs, lib, nodes, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    # Two people's agents: uses of both wait at once (the subtest of four waiting uses).
    torq.agent.people = [ "dev" "sam" ];
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = samHash; };
    torq.broker.rebuildCommand = "true";
    security.pki.certificateFiles = [ "${certs}/ca.crt" ];
    networking.hosts.${nodes.far.networking.primaryIPAddress} = [ "api.example.test" "other.example.test" "git.example.test" ];
    networking.hosts.${nodes.elsewhere.networking.primaryIPAddress} = [ "elsewhere.example.test" ];
    environment.systemPackages = [ pkgs.socat pkgs.curl pkgs.jq windowSet ];
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
        # Their own Authorization is not the one sent: the secret's takes its place. A POST's body goes as it was sent.
        status, _, _ = use("dev", "/user/dev/gh/api.example.test/echo", "-H", "Authorization: Bearer devs-own")
        assert status == 200 and requests()[-1]["headers"]["Authorization"] == "Bearer dev-token-1", requests()[-1]
        status, body, _ = use("dev", "/user/dev/gh/api.example.test/echo", "-X", "POST", "--data-binary", '{"a": 1}')
        assert status == 200 and requests()[-1]["body"] == '{"a": 1}' and requests()[-1]["method"] == "POST", requests()[-1]

    def waiting(person):
        """The uses that wait for `person`, as the broker tells them over its socket."""
        code, v = ask(person, {"args": ["--json", "secret-waiting"]})
        assert code == 0, (code, v)
        return v["waiting"]

    def begin(user, path, *flags, tag=""):
        """A request through the daemon, as `user`, left running: it waits for a person's answer."""
        args = " ".join(shlex.quote(f) for f in flags)
        cmd = f"curl -s -o /tmp/b-{user}{tag} -w '%{{http_code}}' {args} http://127.0.0.1:7800{path} > /tmp/s-{user}{tag}"
        machine.succeed(f"rm -f /tmp/s-{user}{tag}; su -s /bin/sh {user} -c {shlex.quote(cmd)} >&2 &")

    def ended(user, tag=""):
        machine.wait_until_succeeds(f"test -s /tmp/s-{user}{tag}", timeout=60)
        return int(machine.succeed(f"cat /tmp/s-{user}{tag}").strip()), machine.succeed(f"cat /tmp/b-{user}{tag}")

    with subtest("dev's agent account (a coding tool's, not the resident agent) starts tainted: each use waits for dev, and only dev answers"):
        # One agent per person (ADR-051 3): `torq-agent` in dev's rules is dev's agent, torq-agent-dev. Only the
        # resident agent starts a session untainted; every other program of the agent account is tainted from its
        # first request (`not-the-resident-agent`), so its use waits at the broker for dev (the approval design,
        # part 2), under a number, and sends nothing until dev says yes.
        code, s = ask("torq-agent-dev", {"args": ["--json", "session-start"]})
        assert code == 0, (code, s)
        session = s["session"]
        n = len(requests())
        begin("torq-agent-dev", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}", "-H", "X-Torq-Reason: listing the repositories")
        machine.wait_until_succeeds("su - dev -c 'torqcli show system secrets waiting' | grep -q 'ITS SESSION READ UNTRUSTED TEXT'", timeout=30)
        w = waiting("dev")
        assert len(w) == 1 and w[0]["facts"]["secret"] == "gh" and w[0]["facts"]["host"] == "api.example.test" and w[0]["facts"]["tainted"] and w[0]["agent_says"] == "listing the repositories", w
        assert waiting("sam") == [], "asked of dev alone"
        assert len(requests()) == n, "nothing sent while it waits"
        # The agent's own account and another person are refused (dev is this machine's only administrator).
        for other in ["torq-agent-dev", "sam"]:
            code, v = ask(other, {"args": ["--json", "secret-approve", str(w[0]["n"])]})
            assert code == 1, (other, code, v)
        code, v = ask("dev", {"args": ["--json", "secret-deny", str(w[0]["n"])]})
        assert code == 0 and v["kind"] == "secret_use_denied" and v["approved_by"] == "dev" and v["data"]["where"] == "terminal", (code, v)
        status, body = ended("torq-agent-dev")
        assert status == 403 and "denied" in body and "dev-token-1" not in body, (status, body)
        assert len(requests()) == n, "nothing sent"
        # Approved, it is sent, once, and is on the record as approved: never the value.
        begin("torq-agent-dev", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}")
        machine.wait_until_succeeds("su - dev -c 'torqcli show system secrets waiting' | grep -q 'request system secret approve'", timeout=30)
        k = waiting("dev")[0]["n"]
        out = machine.succeed(f"su - dev -c 'torqcli request system secret approve {k}'")
        assert "approved" in out, out
        status, body = ended("torq-agent-dev")
        assert status == 200 and "dev-token-1" not in body and requests()[-1]["headers"]["Authorization"] == "Bearer dev-token-1", (status, body)
        ledger = machine.succeed("torq-broker log --json")
        assert "secret_use_approved" in ledger and "dev-token-1" not in ledger and "listing the repositories" not in ledger

    with subtest("nowhere else, by no one else, and not without a session"):
        n = len(requests())
        for user, path, flags in [
            ("torq-agent-dev", "/user/dev/gh/other.example.test/echo", ["-H", f"X-Torq-Session: {session}"]),
            # No session: dev is asked, as for a tainted one, and with no answer it is refused at its two minutes.
            ("torq-agent-dev", "/user/dev/gh/api.example.test/echo", []),
            ("sam", "/user/dev/gh/api.example.test/echo", []),
            ("dev", "/system/gh/api.example.test/echo", []),
            ("torq-agent-dev", "/user/dev/nothing/api.example.test/echo", ["-H", f"X-Torq-Session: {session}"]),
        ]:
            status, body, _ = use(user, path, *flags)
            assert status == 403 and "dev-token-1" not in body, (user, path, status, body)
        assert len(requests()) == n, "a refused use sends nothing"
        # Bad requests are refused before anything is asked.
        status, _, _ = use("torq-agent-dev", "/user/dev/gh/10.0.0.1/echo")
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

    with subtest("four uses wait at once, from two agents, and hold no connection to the broker: a person's own use is served beside them"):
        # The broker's socket gives one account four connections, and every use comes from the daemon's one
        # account. A use that waits holds none: the daemon is told its number and asks again about it.
        import time
        code, v = ask("sam", {"args": ["--json", "secret-set", "--name", "gh", "--user", "sam"], "key": "sam-token-1"})
        assert code == 0, (code, v)
        diff = f"--- /dev/null\n+++ b/users/sam/secrets.json\n@@ -0,0 +1 @@\n+{rules}\n"
        code, p = ask("sam", {"args": ["--json", "propose", f"--base={head()}", "--reason=gh for sam's agent"], "diff": diff})
        assert code == 0, (code, p)
        code, v = ask("sam", {"args": ["--json", "apply", str(p["seq"])]})
        assert code == 0, (code, v)
        code, sams = ask("torq-agent-sam", {"args": ["--json", "session-start"]})
        assert code == 0, (code, sams)
        for i in range(3):
            begin("torq-agent-dev", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}", tag=f"-w{i}")
        begin("torq-agent-sam", "/user/sam/gh/api.example.test/echo", "-H", f"X-Torq-Session: {sams['session']}", tag="-w3")
        for _ in range(30):
            if len(waiting("dev")) == 3 and len(waiting("sam")) == 1:
                break
            time.sleep(1)
        assert len(waiting("dev")) == 3 and len(waiting("sam")) == 1, (waiting("dev"), waiting("sam"))
        # All four wait. A person's own uses go through beside them, at once, one after another.
        n = len(requests())
        for _ in range(4):
            status, body, _ = use("dev", "/user/dev/gh/api.example.test/echo")
            assert status == 200, (status, body)
        assert len(requests()) == n + 4, "each was sent"
        # A fourth of one agent's is refused at once, in these words; it does not wait.
        status, body, _ = use("torq-agent-dev", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}")
        assert status == 403 and "3 uses are waiting for you already" in body, (status, body)
        for w in waiting("dev"):
            code, v = ask("dev", {"args": ["--json", "secret-deny", str(w["n"])]})
            assert code == 0, (code, v)
        code, v = ask("sam", {"args": ["--json", "secret-approve", str(waiting("sam")[0]["n"])]})
        assert code == 0 and v["kind"] == "secret_use_approved" and v["approved_by"] == "sam", (code, v)
        for i in range(3):
            status, body = ended("torq-agent-dev", tag=f"-w{i}")
            assert status == 403 and "denied" in body, (i, status, body)
        status, body = ended("torq-agent-sam", tag="-w3")
        assert status == 200 and "sam-token-1" not in body and requests()[-1]["headers"]["Authorization"] == "Bearer sam-token-1", (status, body)

    with subtest("a session tainted by what it read too is asked; with no answer in two minutes the use is refused, and recorded as timed out"):
        code, _ = ask("torq-agent-dev", {"args": ["--json", "session-taint", session, "--source", "web"]})
        assert code == 0
        status, body, _ = use("torq-agent-dev", "/user/dev/gh/api.example.test/echo", "-H", f"X-Torq-Session: {session}")
        assert status == 403 and "untrusted" in body and "no one answered" in body, (status, body)
        assert "secret_use_timed_out" in machine.succeed("torq-broker log --json")

    with subtest("an SSH key, from a file, for the host the person checked: its key in the rule (step 3)"):
        machine.succeed("su - dev -c " + shlex.quote("mkdir -p ~/.ssh && ssh-keygen -q -t ed25519 -N \"\" -C deploy -f ~/.ssh/deploy"))
        public = machine.succeed("cat /home/dev/.ssh/deploy.pub").strip()
        for host in [far, elsewhere]:
            host.wait_for_unit("sshd.service")
            host.succeed(f"install -d -o git -m 700 /home/git/.ssh && echo {shlex.quote(public)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys")
        # dev checks git.example.test's key once, as a person does (their own known_hosts), and only that host.
        machine.succeed("su - dev -c 'ssh-keyscan -t ed25519 git.example.test > ~/.ssh/known_hosts 2>/dev/null'")
        machine.succeed("su - dev -c 'torq request system secret set deploy file /home/dev/.ssh/deploy user dev'")
        machine.succeed("su - dev -c 'torq request system secret allow deploy ssh git.example.test user dev'")
        machine.fail("su - dev -c 'torq request system secret allow deploy ssh elsewhere.example.test user dev'")
        machine.succeed("su - dev -c 'torq request system secret allow deploy agent user dev'")
        machine.succeed("su - dev -c 'torq request system secret approve deploy never user dev'")
        rules = json.loads(machine.succeed(f"git -C {repo} show HEAD:users/dev/secrets.json"))
        assert rules["deploy"]["ssh"]["hosts"] == ["git.example.test"] and rules["deploy"]["ssh"]["known"][0].startswith("git.example.test ssh-ed25519 "), rules
        assert machine.succeed("stat -c %a /var/lib/torq/secrets/store/users/dev/deploy.pub").strip() == "644"
        shown = machine.succeed("su - dev -c 'torq show system secrets'")
        assert "deploy" in shown and "SSH key, has a value; ssh to git.example.test" in shown, shown

    with subtest("dev's agent account gets its own agent socket, its key listed, and is refused each signature while it is tainted"):
        code, s = ask("torq-agent-dev", {"args": ["--json", "session-start"]})
        session2 = s["session"]
        out = machine.succeed(f"su -s /bin/sh torq-agent-dev -c \"curl -s -X POST -H 'X-Torq-Session: {session2}' http://127.0.0.1:7800/ssh-agent\"")
        sock = json.loads(out)["socket"]
        assert sock.startswith("/run/torq-secrets/ssh-"), out
        machine.succeed("cp /home/dev/.ssh/known_hosts /tmp/agent-known && chmod 644 /tmp/agent-known")
        ssh = f"SSH_AUTH_SOCK={sock} ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/agent-known -o IdentitiesOnly=no"
        listed = machine.succeed(f"su -s /bin/sh torq-agent-dev -c 'SSH_AUTH_SOCK={sock} ssh-add -l'")
        assert "torq:dev/deploy" in listed, listed
        # Its session started tainted (not the resident agent): each signature waits for dev, who says no here.
        machine.succeed(f"rm -f /tmp/ssh-agent-dev; su -s /bin/sh torq-agent-dev -c '{ssh} git@git.example.test echo signed-in; echo $? > /tmp/ssh-agent-dev' >&2 &")
        machine.wait_until_succeeds("su - dev -c 'torqcli show system secrets waiting' | grep -q 'to sign in over SSH to git.example.test'", timeout=60)
        w = waiting("dev")
        assert len(w) == 1 and w[0]["facts"]["secret"] == "deploy" and w[0]["facts"]["ssh"] and w[0]["facts"]["tainted"], w
        code, v = ask("dev", {"args": ["--json", "secret-deny", str(w[0]["n"])]})
        # Denied by dev for this secret, and on the record (a failed ssh alone could be anything).
        assert code == 0 and v["kind"] == "secret_use_denied" and v["data"]["secret"] == "deploy" and v["data"]["user"] == "dev", (code, v)
        machine.wait_until_succeeds("test -s /tmp/ssh-agent-dev", timeout=60)
        assert machine.succeed("cat /tmp/ssh-agent-dev").strip() != "0", "not signed in"
        journal = machine.succeed("journalctl -u torq-secrets --no-pager")
        assert "torq-agent-dev: " in journal and "denied" in journal, journal
        ledger = machine.succeed("torq-broker log")
        assert "dev's secret deploy for the first time" not in ledger and "PRIVATE KEY" not in ledger, ledger
        # The socket answers its own account alone; it takes no key, nor gives one.
        machine.fail(f"su -s /bin/sh sam -c 'SSH_AUTH_SOCK={sock} ssh-add -l'")
        machine.succeed("su -s /bin/sh torq-agent-dev -c 'ssh-keygen -q -t ed25519 -N \"\" -f /tmp/agents-own'")
        machine.fail(f"su -s /bin/sh torq-agent-dev -c 'SSH_AUTH_SOCK={sock} ssh-add /tmp/agents-own'")
        assert "PRIVATE KEY" not in machine.succeed("journalctl -u torq-secrets --no-pager")

    with subtest("its owner signs in with it the same way"):
        out = machine.succeed("su - dev -c 'curl -s -X POST http://127.0.0.1:7800/ssh-agent'")
        sock = json.loads(out)["socket"]
        assert machine.succeed(f"su - dev -c 'SSH_AUTH_SOCK={sock} ssh -o BatchMode=yes git@git.example.test echo mine'").strip() == "mine"

    with subtest("no one but the daemon asks the broker for a use, and the value is nowhere it can be read"):
        for user in ["torq-agent-dev", "dev", "sam"]:
            code, v = ask(user, {"args": ["--json", "secret-use", "--name", "gh", "--user", "dev", "--host", "api.example.test", "--client-port", "1", "--server-port", "7800"]})
            assert code == 1, (user, code, v)
        pid = machine.succeed("systemctl show -p MainPID --value torq-secrets").strip()
        machine.fail(f"su -s /bin/sh torq-agent-dev -c 'cat /proc/{pid}/environ'")
        machine.fail("su -s /bin/sh torq-agent-dev -c 'ls /var/lib/torq/secrets/store'")
        journal = machine.succeed("journalctl -u torq-secrets --no-pager")
        assert "dev-token-1" not in journal, "never in the daemon's log"
        assert "dev used dev's secret gh" in journal

    with subtest("a value kept the way TorqOS Settings keeps it is usable, and is in no file"):
        # The window's own call (app::secret_set), as dev: over the socket, the value the request's own.
        keep = "su -s /bin/sh {} -c 'torq-secret-set-example {}'"
        said = machine.succeed("printf %s window-token-7 | " + keep.format("dev", "win dev"))
        assert "The value of dev's secret win is kept" in said and "window-token-7" not in said, said
        # Kept by the broker, encrypted: what is in the store is not the value, and decrypts to it.
        store = "/var/lib/torq/secrets/store/users/dev/win"
        assert machine.succeed(f"stat -c %a:%U {store}").strip() == "600:root"
        assert machine.succeed(f"systemd-creds decrypt --name=torq-secret.user.dev.win {store} -") == "window-token-7"
        # Usable: the host is sent it, the program that asked is not.
        machine.succeed("su - dev -c 'torq request system secret allow win https api.example.test user dev'")
        status, body, _ = use("dev", "/user/dev/win/api.example.test/echo")
        assert status == 200 and "window-token-7" not in body, (status, body)
        assert requests()[-1]["headers"]["Authorization"] == "Bearer window-token-7", requests()[-1]
        # In no file of the machine's, the ledger and the logs among them (dev's own torqcli log is in /home).
        found = machine.succeed("grep -rlaF -D skip window-token-7 /home /root /tmp /run /dev/shm /var /etc 2>/dev/null || true")
        assert found.strip() == "", found
        assert "window-token-7" not in machine.succeed("torq-broker log")
        assert "window-token-7" not in machine.succeed("journalctl --no-pager -b")
        # Who may is the broker's to say, this way as any: not another person's, and not the system's for one who is no
        # administrator; nothing typed is no value. What was kept stays.
        machine.fail("printf %s window-token-8 | " + keep.format("sam", "win dev"))
        machine.fail("printf %s window-token-8 | " + keep.format("sam", "win"))
        # dev is an administrator (the owner, in wheel), so the system's is not refused outright: it needs dev's
        # password. This machine has no desktop, so the broker asks no dialog and answers at once that it needs
        # one; there is no terminal to ask at, and the call ends there. Bounded all the same: a wait is a failure.
        code, out = machine.execute("printf %s window-token-8 | timeout 30 " + keep.format("dev", "win") + " 2>&1")
        assert code == 1 and "needs dev's password" in out, (code, out)
        machine.fail(": | " + keep.format("dev", "win dev"))
        assert machine.succeed(f"systemd-creds decrypt --name=torq-secret.user.dev.win {store} -") == "window-token-7"
        machine.fail("test -e /var/lib/torq/secrets/store/system/win")
  '';
}
