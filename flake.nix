{
  description = "Ultron Discord bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # Fixed crane input
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Properly accessing crane's lib
        craneLib = (crane.mkLib pkgs);

        # Common arguments for crane
        commonArgs = {
          # src = craneLib.cleanCargoSource self;
          src = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              (nixpkgs.lib.fileset.maybeMissing ./templates)
            ];
          };

          buildInputs = with pkgs; [
            openssl
            # needed for utoipa
            curl
          ];

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];
        };

        # Build dependencies separately - allows better caching
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual package
        green = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;

            # Additional metadata
            pname = "green";
            version = "0.1.0";

            # Augment wrapper path if needed
            postInstall = ''
              wrapProgram $out/bin/green \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.openssl ]}
            '';
          }
        );
      in
      {
        # Expose the package
        packages = {
          default = green;
          green = green;
        };

        # Add a check to verify the build works
        checks = {
          inherit green;
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          inputsFrom = [ green ];
          packages = with pkgs; [
            rustc
            cargo
            rust-analyzer
            rustfmt
            clippy

            just
            just-lsp
            taplo
            typos
            typos-lsp
          ];
        };
      }
    )
    // {
      # NixOS module that doesn't depend on system
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.services.green;
        in
        {
          options.services.green = with lib; {
            enable = mkEnableOption "Ultron Discord bot";

            package = mkOption {
              type = types.package;
              description = "The green package to use";
              default = self.packages.${pkgs.system}.default;
              defaultText = lib.literalExpression "self.packages.\${pkgs.system}.default";
            };

            user = mkOption {
              type = types.str;
              default = "green";
              description = "User account under which Ultron runs";
            };

            group = mkOption {
              type = types.str;
              default = "green";
              description = "Group under which Ultron runs";
            };

            caPath = mkOption {
              type = types.path;
              example = "/var/lib/green/ca.pem";
              description = "Environment file containing Discord tokens and other secrets";
            };

            port = mkOption {
              type = types.port;
              default = 47336;
              description = "Port to run the server on";
            };

            logLevel = mkOption {
              type = types.str;
              default = "info";
              example = "info,green=debug";
              description = "The log level of the service. See: https://docs.rs/env_logger/latest/env_logger/#enabling-logging";
            };

            routes = mkOption {
              type = with types; attrsOf (submodule {
                options = {
                  url = mkOption {
                    type = types.str;
                    description = "URL for the route.";
                  };
                  description = mkOption {
                    type = types.str;
                    description = "Description of the route.";
                  };
                };
              });
              default = {
                ultron = {
                  url = "ultron.green.chrash.net";
                  description = "Main route for Ultron bot";
                };
                adguard = {
                  url = "adguard.green.chrash.net";
                  description = "AdGuard DNS route";
                };
                grafana = {
                  url = "grafana.green.chrash.net";
                  description = "Grafana monitoring dashboard";
                };
                postgres = {
                  url = "db.green.chrash.net";
                  description = "PostgreSQL database route";
                };
                homeassistant = {
                  url = "homeassistant.green.chrash.net";
                  description = "Home Assistant route";
                };
                frigate = {
                  url = "frigate.green.chrash.net";
                  description = "Frigate for NVR and AI detection";
                };
                foundry = {
                  url = "foundry.green.chrash.net";
                  description = "Foundry Virtual Tabletop route";
                };
              };
              description = "List of routes to register with the bot";
              example = [ {
                url = "example.url";
                description = "Example route description";
              } ];
            };

            dataDir = mkOption {
              type = types.path;
              default = "/var/lib/green";
              description = "Directory where the bot stores its data";
              example = "/var/lib/green";
            };
          };

          config = lib.mkIf cfg.enable {
            users.users = lib.mkIf (cfg.user == "green") {
              green = {
                isSystemUser = true;
                group = cfg.group;
                description = "Ultron Discord bot service user";
                home = cfg.dataDir;
                createHome = true;
              };
            };

            users.groups = lib.mkIf (cfg.group == "green") {
              green = { };
            };

            environment.etc."green/config.toml".text = ''
              port = ${toString cfg.port}
              log_level = "${cfg.logLevel}"
              ca_path = "${cfg.caPath}"

              ${lib.concatStringsSep "\n" (lib.mapAttrsToList (k: v: "[routes.${k}]\nurl = \"${v.url}\"\ndescription = \"${v.description}\"" ) cfg.routes)}
            '';

            systemd.services.green = {
              description = "Ultron Discord bot";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];

              serviceConfig = {
                # Pass CLI arguments based on configuration options
                ExecStart = ''
                  ${cfg.package}/bin/green \
                    --config-path /etc/green/config.toml
                '';
                User = cfg.user;
                Group = cfg.group;
                Restart = "always";
                RestartSec = "10";

                # Data directory
                StateDirectory = baseNameOf cfg.dataDir;
                StateDirectoryMode = "0750";

                # Hardening measures
                CapabilityBoundingSet = "";
                DevicePolicy = "closed";
                LockPersonality = true;
                MemoryDenyWriteExecute = true;
                NoNewPrivileges = true;
                PrivateDevices = true;
                PrivateTmp = true;
                ProtectClock = true;
                ProtectControlGroups = true;
                ProtectHome = true;
                ProtectHostname = true;
                ProtectKernelLogs = true;
                ProtectKernelModules = true;
                ProtectKernelTunables = true;
                ProtectSystem = "strict";
                ReadWritePaths = [ cfg.dataDir ];
                RemoveIPC = true;
                RestrictAddressFamilies = [
                  "AF_INET"
                  "AF_INET6"
                ];
                RestrictNamespaces = true;
                RestrictRealtime = true;
                RestrictSUIDSGID = true;
                SystemCallArchitectures = "native";
                SystemCallFilter = [
                  "@system-service"
                  "~@privileged @resources"
                ];
                UMask = "077";
              };
            };
          };
        };
    };
}
