# NixOS VM test: one agent account per person (ADR-051 3, amending ADR-041 1). Two people, alice and
# bob, each with an agent of their own; dave, a person with none; carol, whose `torq-agent-carol` is an account
# named like an agent that the broker's map does not name. Each claim fails if its guard is taken out:
# bob's agent cannot propose alice's packages (refused, and ledgered as bob's agent's); alice's trust
# does not apply bob's agent's proposal; bob cannot run alice's agent; neither agent reads the other's
# home; a person with no agent account is told so plainly; a look-alike account is no agent.
{ pkgs, self }:

let
  hash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-agents-per-person";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.alice.hashedPasswordFile = lib.mkForce null;
    users.users.alice.hashedPassword = hash;
    # Administrators both, so that what stops bob and carol is the launcher and the broker, not sudo.
    users.users.bob = { isNormalUser = true; extraGroups = [ "wheel" ]; hashedPasswordFile = lib.mkForce null; hashedPassword = hash; };
    users.users.carol = { isNormalUser = true; extraGroups = [ "wheel" ]; hashedPasswordFile = lib.mkForce null; hashedPassword = hash; };
    users.users.dave = { isNormalUser = true; extraGroups = [ "wheel" ]; hashedPasswordFile = lib.mkForce null; hashedPassword = hash; };
    # A person who is not an administrator, with no agent: for who may decline (ADR-051, amended).
    users.users.erin = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = hash; };
    torq.agent.people = [ "alice" "bob" ];
    # Doctor mode on, for its resident service: the trust subtest speaks from that service's control group, as
    # TorqOS's resident agent does (ADR-051 step 5). No model is ever asked here.
    # Its model a socket in a directory only root writes (none listens: no model is asked), so the resident
    # agent's sessions can start clean (a TCP model on this machine would taint them: ADR-051 step 5).
    torq.agent.doctor = { enable = true; endpoint = "unix:/run/torq-test-model/none.sock"; model = "none"; };
    systemd.tmpfiles.rules = [ "d /run/torq-test-model 0755 root root -" ];
    # Named like an agent account, made by hand, not in torq.agent.people: no agent.
    users.users.torq-agent-carol = { isSystemUser = true; group = "torq-agent-carol"; home = "/var/lib/torq-agent-carol"; createHome = true; };
    users.groups.torq-agent-carol = { };
    torq.broker.rebuildCommand = "true";
    environment.systemPackages = [ pkgs.socat pkgs.jq ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")
    PASSWORD = "Torq-sock-2210"

    def as_agent(who, cmd, resident):
        # As the agent account `who`. `resident`: from inside its person's torq-agent@<person>.service's control
        # group, as TorqOS's resident agent is (ADR-051 step 5) — root moves this shell there first, which root may
        # and no process of the agent's can; otherwise from su, as any other program of the agent's is.
        if not resident:
            return machine.succeed(f"su -s /bin/sh {who} -c {shlex.quote(cmd)}")
        unit = f"torq-agent@{who.removeprefix('torq-agent-')}.service"
        machine.succeed(f"systemctl start {unit}")
        cg = machine.succeed(f"systemctl show -P ControlGroup {unit}").strip()
        procs = shlex.quote(f"/sys/fs/cgroup{cg}/cgroup.procs")
        # The resident agent's own account (ADR-051 3 amended 2026-11-11): the agent account is never resident.
        res = who.replace("torq-agent-", "torq-resident-", 1)
        script = f"echo $$ > {procs} && exec setpriv --reuid {res} --regid {res} --init-groups -- env HOME=/var/lib/torq-agent-memory/{who.removeprefix('torq-agent-')}/home sh -c {shlex.quote(cmd)}"
        return machine.succeed(f"sh -c {shlex.quote(script)}")

    def ask(user, request, resident=False):
        cmd = f"printf '%s\\n' {shlex.quote(json.dumps(request))} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        code, out = None, ""
        for l in as_agent(user, cmd, resident).splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            code = f.get("exit", code)
        return code, (json.loads(out) if out.strip() else None)

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def packages(person, names):
        return f"--- /dev/null\n+++ b/users/{person}/packages.json\n@@ -0,0 +1 @@\n+{json.dumps(names)}\n"

    def propose(who, diff, reason, session=None, resident=False):
        args = ["--json", "propose", f"--base={head()}", f"--reason={reason}"] + ([f"--session={session}"] if session else [])
        return ask(who, {"args": args, "diff": diff}, resident)

    # As the resident agent, which alone starts a session untainted (ADR-051 step 5).
    def start(who):
        sid = as_agent(who, "torqcli agent session start", True).strip()
        assert len(sid) == 32, sid
        return sid

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    def run_as(person, cmd):
        return machine.execute(f"su - {person} -c {shlex.quote(cmd)} 2>&1")

    with subtest("each person's agent: its own account, its home its own, the other's out of reach"):
        machine.succeed("jq -e '.agent.agents == {\"torq-agent-alice\": \"alice\", \"torq-agent-bob\": \"bob\"}' /etc/torq/broker.json")
        # Each resident agent's own account, mapped too (ADR-051 3 amended 2026-11-11).
        machine.succeed("jq -e '.residents == {\"torq-resident-alice\": \"alice\", \"torq-resident-bob\": \"bob\"}' /etc/torq/broker.json")
        for a in ["torq-agent-alice", "torq-agent-bob"]:
            machine.succeed(f"getent passwd {a} | grep -q ':/var/lib/{a}:.*nologin$'")
            machine.succeed(f"test \"$(id -u {a})\" -lt 1000 && test \"$(id -Gn {a})\" = {a}")
            machine.succeed(f"passwd -S {a} | grep -q ' L '")
            machine.succeed(f"test \"$(stat -c %a:%U /var/lib/{a})\" = 700:{a}")
        machine.fail("getent passwd torq-agent")
        machine.succeed("su -s /bin/sh torq-agent-alice -c 'echo mine > ~/secret'")
        machine.succeed("su -s /bin/sh torq-agent-bob -c 'echo mine > ~/secret'")
        machine.fail("su -s /bin/sh torq-agent-bob -c 'ls /var/lib/torq-agent-alice'")
        machine.fail("su -s /bin/sh torq-agent-bob -c 'cat /var/lib/torq-agent-alice/secret'")
        machine.fail("su -s /bin/sh torq-agent-alice -c 'cat /var/lib/torq-agent-bob/secret'")
        machine.fail("su -s /bin/sh torq-agent-alice -c 'touch /var/lib/torq-agent-bob/x'")

    with subtest("bob's agent proposes bob's packages, never alice's: refused, and ledgered as his agent's"):
        code, v = propose("torq-agent-bob", packages("bob", ["htop"]), "htop for bob")
        assert code == 0 and v["data"]["agent"] == "torq-agent-bob" and v["data"]["agent_for"] == "bob", (code, v)
        before = head()
        code, v = propose("torq-agent-bob", packages("alice", ["nmap"]), "nmap for alice")
        assert code == 1 and "only for bob" in v["error"], (code, v)
        e = last()
        assert e["kind"] == "refused" and e["actor"] == "agent" and e["data"]["agent"] == "torq-agent-bob" and "users/alice/packages.json" in e["data"]["why"], e
        assert head() == before
        code, v = propose("torq-agent-alice", packages("alice", ["htop"]), "htop for alice")
        assert code == 0 and v["data"]["agent_for"] == "alice", (code, v)

    with subtest("alice's trust applies alice's agent's proposals, not bob's agent's"):
        # What `torqcli request system agent trust 30` sends, with alice's password.
        code, g = ask("alice", {"args": ["--json", "trust-grant", "--minutes=30"], "password": PASSWORD})
        assert code == 0 and g["kind"] == "trust_granted" and g["data"]["person"] == "alice", (code, g)
        sb = start("torq-agent-bob")
        before = head()
        code, v = propose("torq-agent-bob", packages("bob", ["jq"]), "jq for bob, under alice's trust", session=sb, resident=True)
        assert code == 0 and "trust_applied" not in v, (code, v)
        assert head() == before, "bob's agent's proposal was applied under alice's trust"
        # bob's agent cannot cite alice's agent's session, nor report into it.
        sa = start("torq-agent-alice")
        code, v = propose("torq-agent-bob", packages("bob", ["fd"]), "on alice's agent's session", session=sa, resident=True)
        assert code == 1 and "not torq-agent-bob's" in v["error"], (code, v)
        # Refused for that reason, not any (the exit code alone).
        code, out = machine.execute(f"su -s /bin/sh torq-agent-bob -c 'torqcli agent session taint {sa} read_journal' 2>&1")
        assert code != 0 and "not torq-agent-bob's" in out, (code, out)
        machine.succeed(f"jq -e '.tainted == false and .agent == \"torq-agent-alice\"' /var/lib/torq/state/sessions/{sa}.json")
        # alice's own agent, under her trust: applied (the trust works; it is bob's agent it does not cover).
        code, v = propose("torq-agent-alice", packages("alice", ["tree"]), "tree for alice", session=sa, resident=True)
        assert code == 0 and v.get("trust_applied", {}).get("approved_by") == "alice", (code, v)
        machine.succeed(f"git -C {repo} show HEAD:users/alice/packages.json | grep -q tree")

    with subtest("residency from the uid, the control group a second guard"):
        unit = "torq-agent@alice.service"
        machine.succeed(f"systemctl start {unit}")
        # The resident account outside its unit (as root's su makes it): tainted from its start, as any program.
        out = machine.succeed("su -s /bin/sh torq-resident-alice -c 'cd /tmp && HOME=/var/lib/torq-agent-memory/alice/home torqcli agent session start'").strip()
        rec = json.loads(machine.succeed(f"cat /var/lib/torq/state/sessions/{out}.json"))
        assert rec["tainted_by"] == ["not-the-resident-agent"] and rec["agent"] == "torq-agent-alice" and rec["resident"] is False, rec
        # The agent account inside the unit's control group (root moves it there): never resident.
        cg = machine.succeed(f"systemctl show -P ControlGroup {unit}").strip()
        procs = shlex.quote(f"/sys/fs/cgroup{cg}/cgroup.procs")
        sid = machine.succeed(f"sh -c {shlex.quote(f'echo $$ > {procs} && exec setpriv --reuid torq-agent-alice --regid torq-agent-alice --init-groups -- env HOME=/var/lib/torq-agent-alice torqcli agent session start')}").strip()
        rec = json.loads(machine.succeed(f"cat /var/lib/torq/state/sessions/{sid}.json"))
        assert rec["tainted_by"] == ["not-the-resident-agent"] and rec["resident"] is False, rec
        # Both are the person's one agent: one count, one name in the ledger (data.agent), the caller the kernel's.
        rs = start("torq-agent-bob")
        code, v = propose("torq-agent-bob", packages("bob", ["fd"]), "fd for bob, resident", session=rs, resident=True)
        assert code == 0 and v["data"]["agent"] == "torq-agent-bob" and v["data"]["caller"] == "torq-resident-bob" and v["tainted"] is False, (code, v)

    with subtest("torq agent run: each person their own agent, and no one else's"):
        code, out = run_as("bob", "torq agent run -- sh -c 'id -un; echo $HOME; cat /var/lib/torq-agent-alice/secret 2>/dev/null || echo NOPE'")
        assert code == 0 and out.split() == ["torq-agent-bob", "/var/lib/torq-agent-bob", "NOPE"], (code, out)
        code, out = run_as("alice", "torq agent run -- id -un")
        assert code == 0 and out.strip() == "torq-agent-alice", (code, out)
        # The launcher takes whose agent from sudo, never from the caller: bob naming alice gets nothing of hers.
        launcher = machine.succeed("readlink -f /run/current-system/sw/bin/torq-agent-run").strip()
        # Whose agent is sudo's SUDO_UID, which the launcher's rule never lets a caller set (no SETENV,
        # asserted by the module): bob naming alice is refused by sudo itself, and nothing runs.
        alice_uid = machine.succeed("id -u alice").strip()
        code, out = run_as("bob", f"sudo SUDO_USER=alice SUDO_UID={alice_uid} {launcher} -- id -un")
        assert code != 0 and "not allowed to set the following environment variables" in out and "torq-agent-alice" not in out, (code, out)
        code, out = run_as("bob", f"sudo SUDO_UID={alice_uid} {launcher} -- id -un")
        assert code != 0 and "not allowed to set" in out, (code, out)
        code, out = run_as("bob", f"sudo {launcher} -- id -un")
        assert code == 0 and out.strip() == "torq-agent-bob", (code, out)

    with subtest("decline: the person whose agent proposed it, without a password; an administrator, with one; the agent never"):
        code, p = propose("torq-agent-alice", "--- a/users/alice/packages.json\n+++ b/users/alice/packages.json\n@@ -1 +1 @@\n-[\"tree\"]\n+[\"tree\", \"cowsay\"]\n", "cowsay for alice")
        assert code == 0 and p["kind"] == "proposal", (code, p)
        seq = str(p["seq"])
        # alice's agent cannot decline (nor withdraw) it; erin, no administrator, cannot; bob, one, only with his password.
        code, v = ask("torq-agent-alice", {"args": ["--json", "decline", seq]})
        assert code == 1 and "the agent proposes; a person applies" in v["error"], (code, v)
        code, v = ask("erin", {"args": ["--json", "decline", seq]})
        assert code == 1 and "not an administrator" in v["error"], (code, v)
        code, v = ask("bob", {"args": ["--json", "decline", seq]})
        assert code == 77, ("bob is asked his password", code, v)
        # alice: her own agent's proposal, no password.
        code, d = ask("alice", {"args": ["--json", "decline", seq]})
        assert code == 0 and d["kind"] == "declined" and d["approved_by"] == "alice" and d["data"]["proposal"] == p["seq"], (code, d)
        # Never applied after, and no longer waiting.
        code, v = ask("alice", {"args": ["--json", "apply", seq], "password": PASSWORD})
        assert code == 1 and "declined" in v["error"], (code, v)
        machine.fail(f"git -C {repo} show HEAD:users/alice/packages.json | grep -q cowsay")
        assert "cowsay for alice" not in machine.succeed("su - alice -c 'torqcli show system proposals'")
        code, v = ask("alice", {"args": ["--json", "decline", seq]})
        assert code == 1 and "declined already" in v["error"], (code, v)
        # An administrator declines anyone's, with their password: bob, of alice's agent's next one.
        code, p2 = propose("torq-agent-alice", "--- a/users/alice/packages.json\n+++ b/users/alice/packages.json\n@@ -1 +1 @@\n-[\"tree\"]\n+[\"tree\", \"sl\"]\n", "sl for alice")
        assert code == 0, (code, p2)
        code, d = ask("bob", {"args": ["--json", "decline", str(p2["seq"])], "password": PASSWORD})
        assert code == 0 and d["kind"] == "declined" and d["approved_by"] == "bob", (code, d)
        machine.succeed("torq-broker verify")

    with subtest("an agent's proposals waiting at once are bounded, each agent its own count; a decline frees one"):
        # The cap at the running broker, not only in trust_loop. 20 waiting
        # (AGENT_WAITING_MAX); the 100 not done (AGENT_WAITING_LOOK) needs proposals that no longer wait and were
        # never done, which only a hand-written ledger makes: trust_loop's the_not_done_limit_is_exactly_its_number.
        seqs, refused = [], None
        for i in range(25):
            code, p = propose("torq-agent-bob", packages("bob", [f"cap{i}"]), f"cap {i}")
            if code != 0:
                refused = p
                break
            seqs.append(str(p["seq"]))
        assert refused is not None and "waiting" in refused["error"] and len(seqs) <= 20, (len(seqs), refused)
        # Alice's agent is not held up by bob's.
        code, p = propose("torq-agent-alice", "--- a/users/alice/packages.json\n+++ b/users/alice/packages.json\n@@ -1 +1 @@\n-[\"tree\"]\n+[\"tree\", \"figlet\"]\n", "figlet for alice")
        assert code == 0 and p["kind"] == "proposal", (code, p)
        code, d = ask("bob", {"args": ["--json", "decline", seqs[0]]})
        assert code == 0 and d["kind"] == "declined", (code, d)
        code, p = propose("torq-agent-bob", packages("bob", ["one-more"]), "one more, now")
        assert code == 0 and p["kind"] == "proposal", (code, p)

    with subtest("a person with no agent account is told so; a look-alike account is no agent"):
        # dave has no agent account at all: torqcli says so before anything runs.
        code, out = run_as("dave", "torq agent run -- id -un")
        assert code != 0 and "you have no agent account on this machine (torq-agent-dave)" in out, (code, out)
        # carol's look-alike account exists, but the broker's map does not name it: the launcher refuses.
        code, out = run_as("carol", "torq agent run -- id -un")
        assert code != 0 and "carol has no agent account" in out, (code, out)
        # Straight to the launcher (torq-agent-carol exists, but the broker's map does not name it).
        code, out = run_as("carol", f"sudo {launcher} -- id -un")
        assert code != 0 and "carol has no agent account" in out, (code, out)
        machine.fail("pgrep -u torq-agent-carol")
        # At the broker: not an agent, not a person — it proposes nothing and starts no session.
        code, v = propose("torq-agent-carol", packages("carol", ["htop"]), "an impostor")
        assert code == 1 and "not a person's account" in v["error"], (code, v)
        code, v = ask("torq-agent-carol", {"args": ["--json", "session-start"]})
        assert code == 1 and "not a person's account" in v["error"], (code, v)
        machine.succeed("torq-broker verify")
  '';
}
