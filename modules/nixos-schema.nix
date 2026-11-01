# Every NixOS setting of this system, for torqcli (docs/CONFIG-IS-THE-DEVICE.md): its path, its type as
# structure (not the documentation's words) and one line of help, generated from the system's own
# options — TorqOS's `torq.*` included — and installed as /etc/torq/nixos-schema.json. So the
# configuration tree follows NixOS as it changes, with nothing written per setting.
#
# Read as NixOS's documentation is (lib.optionAttrSetToDocList): every option, then those inside its
# submodules, `<name>` for a named entry (`services.httpd.virtualHosts.<name>.forceSSL`). Hidden
# options (`visible = false`, `internal`) are left out with all below them; `visible = "shallow"`
# keeps the option but not what is below it (a whole nested NixOS, e.g. specialisation.<name>).
#
# Like the hand-made schema (schema.nix), this is not the authority: the options' own types are,
# when the system is evaluated. Anything torqcli checks early from this file is checked again there.
{ options, config, lib, pkgs, ... }:

let
  # A type as data: { name, elem?, left?, right?, values? }. Depth-limited: some types are
  # recursive (JSON-like formats), and five levels say all torqcli uses.
  typeOf = depth: t:
    let
      nested = t.nestedTypes or { };
      sub = k: if depth > 0 && nested ? ${k} then typeOf (depth - 1) nested.${k} else null;
      payload = t.functor.payload or null;
      values =
        if builtins.isAttrs payload && payload ? values then payload.values
        else if builtins.isList payload then payload
        else null;
      plain = v: builtins.isString v || builtins.isInt v;
      # A range's bounds are only in its description ("integer between 1 and 65535 (both inclusive)").
      bounds = builtins.match "[^0-9-]*(-?[0-9]+) and (-?[0-9]+).*" (t.description or "");
    in
    lib.filterAttrs (_: v: v != null) {
      name = t.name or "unknown";
      elem = sub "elemType";
      left = sub "left";
      right = sub "right";
      values = if (t.name or "") == "enum" && builtins.isList values && builtins.all plain values then values else null;
      range = if builtins.elem (t.name or "") [ "intBetween" "numberBetween" ] && bounds != null then map lib.toInt bounds else null;
    };

  # The first paragraph of a description, on one line, at most 160 characters.
  help = d:
    let
      text = if builtins.isAttrs d then d.text or "" else if builtins.isString d then d else "";
      first = builtins.head (lib.splitString "\n\n" (lib.trim text));
    in
    builtins.substring 0 160 (builtins.replaceStrings [ "\n" ] [ " " ] first);

  # Where this system's options sit in whatever evaluates it (a test evaluates each machine at
  # `nodes.<name>`): paths are written from the system itself.
  prefix = builtins.length options.networking.hostName.loc - 2;

  walk = opts: builtins.concatMap (opt:
    let
      visible = opt.visible or true;
      # (A read-only setting is NixOS's own to set: never offered, never settable.)
      shown = visible != false && !(opt.internal or false) && !(opt.readOnly or false);
      subs = builtins.tryEval (opt.type.getSubOptions opt.loc);
      below = if visible == true && subs.success && subs.value != { } then walk subs.value else [ ];
      entry = { loc = lib.drop prefix opt.loc; type = typeOf 4 opt.type; help = help (opt.description or ""); };
      # A section that takes any key (RFC 42's `settings`: a freeform type, attrsOf of what each key
      # takes): where, in the tree, and the type of each key's value. Where entries are named (an
      # attrsOf or listOf of submodules), at the entry.
      free = builtins.tryEval (if subs.success then (subs.value._module.freeformType.value or null) else null);
      at = opt.loc ++ (if builtins.elem (opt.type.name or "") [ "attrsOf" "lazyAttrsOf" ] then [ "<name>" ] else if (opt.type.name or "") == "listOf" then [ "*" ] else [ ]);
      freeform =
        if visible == true && free.success && free.value != null && builtins.elem (free.value.name or "") [ "attrsOf" "lazyAttrsOf" ] && free.value.nestedTypes ? elemType
        then [ { loc = lib.drop prefix at; free = typeOf 4 free.value.nestedTypes.elemType; } ]
        else [ ];
    in
    (if shown then [ entry ] ++ freeform else [ ]) ++ below
  ) (lib.collect lib.isOption opts);
in
{
  options.torq.nixosSchema.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = "Install /etc/torq/nixos-schema.json: every NixOS setting of this system, for torqcli.";
  };

  config = lib.mkIf config.torq.nixosSchema.enable {
    environment.etc."torq/nixos-schema.json".source =
      pkgs.writeText "torq-nixos-schema.json" (builtins.toJSON {
        version = 1;
        settings = walk options;
        # What the tree may never change (settings-fence.nix), for torqcli to refuse early.
        fence = (import ./settings-fence.nix { inherit lib; }).patterns;
        # A person's password: never in the tree (settings-fence.nix, rule 3).
        passwords = (import ./settings-fence.nix { inherit lib; }).passwords;
      });
  };
}
