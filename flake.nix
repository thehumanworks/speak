{
  description = "speak CLI with a separately installed, pinned ONNX Runtime";

  # Immutable nixos-26.05 revision: ONNX Runtime 1.24.4 for ort 2.0.0-rc.12
  # (API 24, Rust >= 1.88). Do not follow a consumer's unrelated nixpkgs input.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/718f670e845328e9c2cecdd832835712b2cbd065";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;
          runtime = pkgs.onnxruntime.override {
            pythonSupport = false;
            cudaSupport = false;
            rocmSupport = false;
            coremlSupport = false;
          };
          # ALSA_PLUGIN_DIR is one directory, not a colon-separated search path.
          # Include both backends so existing PulseAudio/WSLg and PipeWire
          # defaults resolve their plugins without changing device routing.
          alsaPlugins = pkgs.symlinkJoin {
            name = "speak-alsa-plugins";
            paths = [
              "${pkgs.alsa-plugins}/lib/alsa-lib"
              "${lib.getLib pkgs.pipewire}/lib/alsa-lib"
            ];
          };
          speak = assert runtime.version == "1.24.4";
            pkgs.rustPlatform.buildRustPackage {
              pname = "speak";
              version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
              src = lib.cleanSource ./.;
              cargoLock.lockFile = ./Cargo.lock;

              nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
              buildInputs = [ runtime pkgs.alsa-lib pkgs.openssl ];
              buildNoDefaultFeatures = true;
              buildFeatures = [ "download" ];

              # ort-sys rc.12 exits through its system dynamic-link path before
              # its downloader. The skip flag also prevents download fallback.
              # Nix's linker records the runtime store path in the ELF RPATH.
              env = {
                ORT_LIB_PATH = "${lib.getLib runtime}/lib";
                ORT_PREFER_DYNAMIC_LINK = "1";
                ORT_SKIP_DOWNLOAD = "1";
                CARGO_NET_OFFLINE = "true";
              };

              # Only supply missing plugin/certificate paths. Do not set an ALSA
              # default device, overwrite user settings or start an audio daemon.
              postInstall = ''
                wrapProgram "$out/bin/speak" \
                  --set-default ALSA_PLUGIN_DIR "${alsaPlugins}" \
                  --set-default SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              '';

              doInstallCheck = true;
              nativeInstallCheckInputs = [ pkgs.patchelf ];
              installCheckPhase = ''
                runHook preInstallCheck
                test -e "${alsaPlugins}/libasound_module_pcm_pulse.so"
                test -e "${alsaPlugins}/libasound_module_pcm_pipewire.so"
                # Prove ORT is dynamically linked and retained in the closure.
                patchelf --print-needed "$out/bin/.speak-wrapped" | grep -F libonnxruntime.so
                patchelf --print-rpath "$out/bin/.speak-wrapped" | grep -F '${lib.getLib runtime}/lib'
                "$out/bin/speak" --list-voices
                "$out/bin/speak" --dump-chunks "Nix smoke test."
                runHook postInstallCheck
              '';

              passthru.onnxruntime = runtime;
              meta = {
                description = "On-device Supertonic-3 text-to-speech CLI";
                homepage = "https://github.com/thehumanworks/speak";
                license = lib.licenses.mit;
                mainProgram = "speak";
                platforms = systems;
              };
            };
        in { inherit speak; default = speak; });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.speak}/bin/speak";
        };
      });

      checks = forAllSystems (system: {
        package = self.packages.${system}.speak;
      });
    };
}
