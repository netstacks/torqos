{ config, lib, ... }:

let
  cfg = config.kestrel.ai;
in
{
  options.kestrel.ai = {
    enable = lib.mkEnableOption "the Kestrel AI agent (TODO(M2))";

    modelEndpoint = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "http://192.168.64.1:8080";
      description = ''
        OpenAI-compatible endpoint of the model the agent uses. During development this is
        llama.cpp / Ollama running on the Mac host; on the reference laptop it is local.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.modelEndpoint != null;
      message = "kestrel.ai.modelEndpoint must be set when kestrel.ai.enable is true.";
    }];

    environment.variables.KESTREL_MODEL_ENDPOINT = cfg.modelEndpoint;

    # TODO(M2): systemd.services.kestrel-agent — unprivileged, sandboxed (DynamicUser,
    # ProtectSystem=strict, bubblewrap), read-only MCP tools, talks to the broker over a socket.
  };
}
