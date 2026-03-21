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

        # Rust toolchain with llvm-tools for coverage instrumentation.
        # All dev tools (rust-analyzer, rustfmt, clippy) are bundled here so
        # the devShell gets a single coherent toolchain rather than mixing
        # nixpkgs Rust packages with the rust-overlay toolchain.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "llvm-tools"   # required by cargo-llvm-cov
            "rust-src"     # required by rust-analyzer
            "rust-analyzer"
            "rustfmt"
            "clippy"
          ];
        };

        # Crane library pinned to our toolchain so that builds, tests, and
        # coverage all use the same Rust/LLVM version.
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common arguments for crane
        commonArgs = {
          # src = craneLib.cleanCargoSource self;
          src = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              (nixpkgs.lib.fileset.maybeMissing ./templates)
              (nixpkgs.lib.fileset.maybeMissing ./migrations)
              (nixpkgs.lib.fileset.maybeMissing ./fixtures)
              (nixpkgs.lib.fileset.maybeMissing ./assets)
              ./justfile
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
            just
            nushell
          ];
        };

        # Build dependencies separately — allows better caching across check
        # derivations that share the same source + flags.
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual package
        green = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;

            # Additional metadata
            pname = "green";
            version = "0.1.0";

            postInstall = ''
              mkdir -p $out/share/green
              cp -r assets $out/share/green/assets
              wrapProgram $out/bin/green \
                --chdir "$out/share/green" \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.openssl ]}
            '';
          }
        );

        # Run tests with nextest (faster, better output than cargo test).
        green-nextest = craneLib.cargoNextest (
          commonArgs
          // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          }
        );

        # Coverage check: threshold is defined in justfile (coverage_threshold).
        # just is a nativeBuildInput so the sandbox can call `just coverage-nix`.
        green-coverage = craneLib.cargoLlvmCov (
          commonArgs
          // {
            inherit cargoArtifacts;
            nativeBuildInputs = (commonArgs.nativeBuildInputs or [ ]) ++ [ pkgs.cargo-nextest ];
            buildPhaseCargoCommand = "just coverage-nix $out";
          }
        );

        # Fixed-output derivation: pre-fetch all JS/Deno dependencies.
        # Network access is allowed here; the output hash pins exact contents.
        #
        # To update after bumping deps in deno.json / deno.lock:
        #   nix build .#packages.x86_64-linux.deno-deps --rebuild 2>&1 | grep "got:"
        # then replace outputHash with the printed value.
        denoPackageCache = pkgs.stdenv.mkDerivation {
          name = "green-js-deps";

          src = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              ./deno.json
              ./deno.lock
              ./package.json
            ];
          };

          nativeBuildInputs = [ pkgs.deno ];

          outputHashMode = "recursive";
          outputHashAlgo = "sha256";
          outputHash = "sha256-km152hKHFVoelXhP1odT1Nr80bGs2NVuCuKnlzKocgc=";

          unpackPhase = ''
            cp -r $src/. .
            chmod -R +w .
          '';

          buildPhase = ''
            export HOME=$TMPDIR
            export DENO_DIR=$TMPDIR/deno-dir
            deno install --frozen
          '';

          installPhase = ''
            mkdir -p $out
            cp -r node_modules $out/node_modules
            cp -r $DENO_DIR $out/deno-dir
          '';
        };

        # Run JS tests inside the Nix sandbox using pre-fetched deps.
        green-js-test = pkgs.runCommand "green-js-test"
          {
            src = nixpkgs.lib.fileset.toSource {
              root = ./.;
              fileset = nixpkgs.lib.fileset.unions [
                ./deno.json
                ./deno.lock
                ./src/js
                ./test/js
                ./justfile
              ];
            };
            nativeBuildInputs = [ pkgs.deno pkgs.just pkgs.nushell ];
          }
          ''
            cp -r $src/. .
            chmod -R +w .

            cp -r ${denoPackageCache}/node_modules ./node_modules
            export DENO_DIR=$TMPDIR/deno-dir
            cp -r ${denoPackageCache}/deno-dir $DENO_DIR
            chmod -R +w $DENO_DIR
            export HOME=$TMPDIR

            just js-test
            touch $out
          '';
      in
      {
        packages = {
          default = green;
          inherit green;
          deno-deps = denoPackageCache;
        };

        # checks run during `nix flake check` and `nix build .#checks.<system>.*`
        checks = {
          inherit green green-nextest green-coverage green-js-test;
        };

        # Development shell — uses the same coherent rustToolchain as crane so
        # that `cargo llvm-cov nextest` works out of the box without any extra
        # rustup component installation.
        devShells.default = pkgs.mkShell {
          inputsFrom = [ green ];
          packages = [
            rustToolchain

            pkgs.cargo-nextest
            pkgs.cargo-llvm-cov

            pkgs.just
            pkgs.just-lsp
            pkgs.taplo
            pkgs.typos
            pkgs.typos-lsp
            pkgs.biome
            pkgs.deno
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
              type = types.attrsOf (types.submodule {
                options = {
                  url = mkOption {
                    type = types.str;
                    description = "URL for this route";
                  };
                  description = mkOption {
                    type = types.str;
                    description = "Description of this route";
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

            auth = mkOption {
              default = null;
              description = "WebAuthn / passkey authentication configuration. If null, auth routes are disabled.";
              type = types.nullOr (types.submodule {
                options = {
                  rpId = mkOption {
                    type = types.str;
                    example = "example.com";
                    description = "WebAuthn relying party ID (typically the registrable domain suffix)";
                  };
                  rpOrigin = mkOption {
                    type = types.str;
                    example = "https://green.example.com";
                    description = "WebAuthn relying party origin URL";
                  };
                  dbUrl = mkOption {
                    type = types.str;
                    example = "postgres://green:password@localhost/green";
                    description = "PostgreSQL connection URL for user and passkey storage";
                  };
                  gmUsers = mkOption {
                    type = types.listOf types.str;
                    default = [];
                    example = [ "alice" ];
                    description = "Usernames that receive the GM role";
                  };
                  ntfyUrl = mkOption {
                    type = types.nullOr types.str;
                    default = null;
                    example = "https://ntfy.example.com/my-secret-topic";
                    description = "ntfy topic URL for sending account recovery codes. If null, recovery notifications are not sent.";
                  };
                  dbUrlFile = mkOption {
                    type = types.nullOr types.path;
                    default = null;
                    example = "/run/secrets/green-db-url";
                    description = ''
                      Path to a file containing <literal>GREEN_DB_URL=postgres://...</literal>.
                      When set, this overrides <option>dbUrl</option> at runtime so that
                      credentials never appear in the Nix store.
                    '';
                  };
                };
              });
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

              ${lib.concatStringsSep "\n" (lib.mapAttrsToList (k: v: "[routes.${k}]\nurl = \"${v.url}\"\ndescription = \"${v.description}\"") cfg.routes)}

              ${lib.optionalString (cfg.auth != null) ''
              [auth]
              rp_id = "${cfg.auth.rpId}"
              rp_origin = "${cfg.auth.rpOrigin}"
              db_url = "${cfg.auth.dbUrl}"
              gm_users = [${lib.concatStringsSep ", " (map (u: "\"${u}\"") cfg.auth.gmUsers)}]
              ${lib.optionalString (cfg.auth.ntfyUrl != null) "ntfy_url = \"${cfg.auth.ntfyUrl}\""}
              ''}
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
                EnvironmentFile = lib.optional
                  (cfg.auth != null && cfg.auth.dbUrlFile != null)
                  cfg.auth.dbUrlFile;
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
